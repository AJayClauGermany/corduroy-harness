//! The host side of each capability: what actually happens when a plugin calls an import.
//!
//! Every function here follows the same shape: work out which resource the plugin is asking
//! for, check it against the plugin's grants, then do the work. A refusal is reported to the
//! plugin as `denied` and recorded on the event stream as `capability/denied`.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use corduroy_core::{
    CallError as CoreCallError, Draft, Gates, Grants, Kernels, Services, Source, TOOL_CALL,
    ToolError as CoreToolError, Tools, Verdict, json,
};
use serde_json::{Value, json};
use wasmtime::StoreLimits;

use super::bindings::corduroy::plugin::types::{CallError, HostError, ToolError, ToolSpec};
use super::bindings::corduroy::plugin::{
    clock, events, fs_read, fs_write, kernel, log, random, service, state, tools, types,
};
use super::paths;

/// Largest file `fs-read` will hand to a plugin, in bytes.
const MAX_FILE: u64 = 8 << 20;

/// Longest message `log` records, in bytes. Longer ones are cut short.
const MAX_LOG: usize = 16 << 10;

/// Most bytes `random` hands out per call.
const MAX_RANDOM: u32 = 64 << 10;

/// Longest key `state` stores, in bytes.
const MAX_KEY: usize = 256;

/// Longest value `state` stores, in bytes.
const MAX_VALUE: usize = 64 << 10;

/// What the host lends a plugin's instances: the registries its capabilities reach, and the
/// plugin's `state` store. Shared by every instance of one application of the plugin.
#[derive(Clone, Default)]
pub(super) struct Lent {
    pub(super) kernels: Option<Arc<Kernels>>,
    pub(super) tools: Option<Arc<Tools>>,
    pub(super) gates: Option<Arc<Gates>>,
    pub(super) services: Option<Arc<Services>>,
    /// The `state` store as of the last call that returned.
    pub(super) state: Arc<Mutex<BTreeMap<String, String>>>,
}

impl Lent {
    pub(super) fn state(&self) -> MutexGuard<'_, BTreeMap<String, String>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Everything the host knows about one plugin instance. Lives in the instance's store.
pub(super) struct HostState {
    /// Name used as the source of the plugin's events.
    pub(super) plugin: String,
    pub(super) grants: Grants,
    /// Directory relative paths are resolved against.
    pub(super) root: PathBuf,
    pub(super) lent: Lent,
    /// Events raised during the current call, delivered once it returns. Delivering them at
    /// once could re-enter the instance that is still running.
    pub(super) outbox: Vec<Draft>,
    /// `state` writes of the current call, by key (`None` deletes). Applied to the store and
    /// recorded only if the call returns.
    pub(super) pending: BTreeMap<String, Option<String>>,
    /// The `state/set` and `state/remove` events recording `pending`.
    pub(super) writes: Vec<Draft>,
    /// Tool calls this instance has made, for their ids.
    pub(super) calls: u64,
    pub(super) limits: StoreLimits,
    /// When the instance was made: the zero of its monotonic clock.
    pub(super) started: Instant,
}

impl HostState {
    fn check(&mut self, capability: &str, resource: &str) -> Result<(), HostError> {
        self.grants.check(capability, resource).map_err(|denied| {
            let payload = format!(
                "{{\"plugin\":{},\"capability\":{},\"resource\":{}}}",
                json::string(&self.plugin),
                json::string(capability),
                json::string(resource)
            );
            self.outbox
                .push(Draft::new(Source::Host, "capability/denied").payload(payload));
            HostError::Denied(denied.to_string())
        })
    }

    /// Resolve a path named by the plugin and check it against `capability`.
    fn path(&mut self, capability: &str, path: &str) -> Result<PathBuf, HostError> {
        let resolved = paths::resolve(&self.root, path);
        self.check(capability, &paths::scope_key(&resolved))?;
        Ok(resolved)
    }
}

fn failed(path: &str, error: &std::io::Error) -> HostError {
    HostError::Failed(format!("{path}: {error}"))
}

impl types::Host for HostState {}

impl fs_read::Host for HostState {
    fn read(&mut self, path: String) -> Result<String, HostError> {
        let resolved = self.path("fs-read", &path)?;
        let size = fs::metadata(&resolved)
            .map_err(|error| failed(&path, &error))?
            .len();
        if size > MAX_FILE {
            return Err(HostError::Failed(format!(
                "{path}: {size} bytes is over the {MAX_FILE} byte limit"
            )));
        }
        fs::read_to_string(&resolved).map_err(|error| failed(&path, &error))
    }

    fn list(&mut self, path: String) -> Result<Vec<String>, HostError> {
        let resolved = self.path("fs-read", &path)?;
        let mut names = Vec::new();
        for entry in fs::read_dir(&resolved).map_err(|error| failed(&path, &error))? {
            let entry = entry.map_err(|error| failed(&path, &error))?;
            let mut name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_dir() {
                name.push('/');
            }
            names.push(name);
        }
        names.sort();
        Ok(names)
    }
}

impl fs_write::Host for HostState {
    fn write(&mut self, path: String, contents: String) -> Result<(), HostError> {
        let resolved = self.path("fs-write", &path)?;
        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent).map_err(|error| failed(&path, &error))?;
        }
        fs::write(&resolved, contents).map_err(|error| failed(&path, &error))
    }

    fn remove(&mut self, path: String) -> Result<(), HostError> {
        let resolved = self.path("fs-write", &path)?;
        fs::remove_file(&resolved).map_err(|error| failed(&path, &error))
    }
}

impl kernel::Host for HostState {
    fn exec(
        &mut self,
        kernel: String,
        code: String,
        timeout_ms: u32,
    ) -> Result<kernel::Cell, HostError> {
        self.check("kernel", &kernel)?;
        let kernels = self
            .lent
            .kernels
            .as_ref()
            .ok_or_else(|| HostError::Failed("the host provides no kernels".to_owned()))?;
        let timeout = Duration::from_millis(u64::from(timeout_ms));
        kernels
            .exec(&kernel, &code, timeout)
            .map(|cell| kernel::Cell {
                output: cell.output,
                status: cell.status,
                truncated: cell.truncated,
            })
            .map_err(|error| HostError::Failed(error.to_string()))
    }
}

impl events::Host for HostState {
    fn emit(&mut self, topic: String, payload: String) -> Result<(), HostError> {
        self.check("events", &topic)?;
        let source = Source::Plugin(self.plugin.clone());
        self.outbox.push(Draft::new(source, topic).payload(payload));
        Ok(())
    }
}

impl log::Host for HostState {
    fn write(&mut self, level: log::Level, mut message: String) -> Result<(), HostError> {
        let level = match level {
            log::Level::Debug => "debug",
            log::Level::Info => "info",
            log::Level::Warn => "warn",
            log::Level::Error => "error",
        };
        self.check("log", level)?;
        let truncated = message.len() > MAX_LOG;
        if truncated {
            message.truncate(message.floor_char_boundary(MAX_LOG));
        }
        let payload = format!(
            "{{\"level\":{},\"message\":{},\"truncated\":{truncated}}}",
            json::string(level),
            json::string(&message)
        );
        let source = Source::Plugin(self.plugin.clone());
        self.outbox
            .push(Draft::new(source, "plugin/log").payload(payload));
        Ok(())
    }
}

impl clock::Host for HostState {
    fn now(&mut self) -> Result<clock::Datetime, HostError> {
        self.check("clock", "wall")?;
        let since = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|error| HostError::Failed(format!("the clock is before 1970: {error}")))?;
        Ok(clock::Datetime {
            seconds: since.as_secs(),
            nanoseconds: since.subsec_nanos(),
        })
    }

    fn monotonic(&mut self) -> Result<u64, HostError> {
        self.check("clock", "monotonic")?;
        // Saturates after some 584 years of one instance.
        Ok(u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX))
    }
}

impl random::Host for HostState {
    fn get_bytes(&mut self, len: u32) -> Result<Vec<u8>, HostError> {
        self.check("random", "secure")?;
        if len > MAX_RANDOM {
            return Err(HostError::Failed(format!(
                "{len} bytes is over the {MAX_RANDOM} byte limit"
            )));
        }
        let mut bytes = vec![0; usize::try_from(len).unwrap_or(0)];
        getrandom::fill(&mut bytes)
            .map_err(|error| HostError::Failed(format!("no random bytes: {error}")))?;
        Ok(bytes)
    }
}

/// A registry the host was to lend and did not: a host misconfiguration, not the plugin's.
fn unlent(what: &str) -> String {
    format!("the host provides no {what}")
}

impl HostState {
    /// An event from this plugin.
    fn draft(&self, topic: &str, payload: &Value) -> Draft {
        Draft::new(Source::Plugin(self.plugin.clone()), topic).payload(payload.to_string())
    }

    /// Ask the gates about this plugin's call `id` to `tool`, recording their decision. Returns
    /// why the call may not run, if it may not.
    fn gate(&mut self, id: &str, tool: &str, input: &Value, read_only: bool) -> Option<String> {
        let caller = Source::Plugin(self.plugin.clone()).to_string();
        let request = json!({
            "session": null, "id": id, "tool": tool, "input": input,
            "read-only": read_only, "caller": caller,
        });
        let decision = self
            .lent
            .gates
            .as_ref()?
            .decide(TOOL_CALL, &request.to_string())?;
        let recorded = json!({
            "point": TOOL_CALL, "id": id, "tool": tool, "verdict": decision.verdict.kind(),
            "reason": decision.verdict.reason(), "gate": decision.gate, "caller": caller,
        });
        self.outbox
            .push(Draft::new(Source::Host, "gate/decision").payload(recorded.to_string()));
        match decision.verdict {
            Verdict::Allow => None,
            Verdict::Deny(reason) => Some(format!("denied by gate `{}`: {reason}", decision.gate)),
            Verdict::Ask(reason) => Some(format!(
                "gate `{}` asked for a person's approval, which a plugin's call cannot wait \
                 for: {reason}",
                decision.gate
            )),
        }
    }
}

impl tools::Host for HostState {
    fn list(&mut self) -> Vec<ToolSpec> {
        let Some(registry) = self.lent.tools.as_ref() else {
            return Vec::new();
        };
        registry
            .specs()
            .into_iter()
            .filter(|spec| self.grants.check("tools", &spec.name).is_ok())
            .map(|spec| ToolSpec {
                name: spec.name,
                description: spec.description,
                input_schema: spec.input_schema,
                replay_safe: spec.replay_safe,
                read_only: spec.read_only,
            })
            .collect()
    }

    fn call(&mut self, name: String, input: String) -> Result<String, ToolError> {
        self.check("tools", &name).map_err(|_| {
            ToolError::Denied(format!("capability `tools` does not cover `{name}`"))
        })?;
        let registry = self
            .lent
            .tools
            .clone()
            .ok_or_else(|| ToolError::Failed(unlent("tools")))?;
        let tool = registry
            .get(&name)
            .ok_or_else(|| ToolError::UnknownTool(name.clone()))?;
        let arguments: Value = serde_json::from_str(&input)
            .map_err(|error| ToolError::InvalidInput(error.to_string()))?;
        self.calls = self.calls.saturating_add(1);
        let id = format!("plugin:{}:{}", self.plugin, self.calls);
        let call = self.draft(
            "tool/call",
            &json!({"id": id, "tool": name, "input": arguments}),
        );
        self.outbox.push(call);
        if let Some(refusal) = self.gate(&id, &name, &arguments, tool.spec().read_only) {
            let result = json!({"id": id, "tool": name, "ok": false, "output": refusal});
            self.outbox.push(self.draft("tool/result", &result));
            return Err(ToolError::Denied(refusal));
        }
        let outcome = tool.call(&input);
        let (ok, output) = match &outcome {
            Ok(output) => (true, output.clone()),
            Err(error) => (false, error.to_string()),
        };
        let result = json!({"id": id, "tool": name, "ok": ok, "output": output});
        self.outbox.push(self.draft("tool/result", &result));
        outcome.map_err(|error| match error {
            CoreToolError::UnknownTool(name) => ToolError::UnknownTool(name),
            CoreToolError::InvalidInput(reason) => ToolError::InvalidInput(reason),
            CoreToolError::Failed(reason) => ToolError::Failed(reason),
            CoreToolError::Denied(reason) => ToolError::Denied(reason),
        })
    }
}

impl state::Host for HostState {
    fn get(&mut self, key: String) -> Result<Option<String>, HostError> {
        self.check("state", &key)?;
        if let Some(pending) = self.pending.get(&key) {
            return Ok(pending.clone());
        }
        Ok(self.lent.state().get(&key).cloned())
    }

    fn set(&mut self, key: String, value: String) -> Result<(), HostError> {
        self.check("state", &key)?;
        if key.len() > MAX_KEY || value.len() > MAX_VALUE {
            return Err(HostError::Failed(format!(
                "keys are at most {MAX_KEY} bytes and values at most {MAX_VALUE}"
            )));
        }
        let write = self.draft("state/set", &json!({"key": key, "value": value}));
        self.writes.push(write);
        self.pending.insert(key, Some(value));
        Ok(())
    }

    fn remove(&mut self, key: String) -> Result<(), HostError> {
        self.check("state", &key)?;
        let write = self.draft("state/remove", &json!({"key": key}));
        self.writes.push(write);
        self.pending.insert(key, None);
        Ok(())
    }

    fn keys(&mut self) -> Result<Vec<String>, HostError> {
        let mut keys: Vec<String> = self.lent.state().keys().cloned().collect();
        for (key, value) in &self.pending {
            match value {
                Some(_) if !keys.contains(key) => keys.push(key.clone()),
                None => keys.retain(|kept| kept != key),
                Some(_) => {}
            }
        }
        keys.retain(|key| self.grants.check("state", key).is_ok());
        keys.sort();
        Ok(keys)
    }
}

impl service::Host for HostState {
    fn call(
        &mut self,
        service: String,
        method: String,
        payload: String,
    ) -> Result<String, CallError> {
        let name = service;
        self.check("service", &name).map_err(|_| {
            CallError::Denied(format!("capability `service` does not cover `{name}`"))
        })?;
        let services = self
            .lent
            .services
            .clone()
            .ok_or_else(|| CallError::Failed(unlent("services")))?;
        services
            .call(&self.plugin, &name, &method, &payload)
            .map_err(|error| match error {
                CoreCallError::Unknown(what) => CallError::Unknown(what),
                CoreCallError::InvalidInput(reason) => CallError::InvalidInput(reason),
                CoreCallError::Failed(reason) => CallError::Failed(reason),
                CoreCallError::Denied(reason) => CallError::Denied(reason),
            })
    }
}
