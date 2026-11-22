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
    CallError as CoreCallError, Completion, Draft, Event, EventId, EventStream, FiberId, Gates,
    Grants, HttpClients, HttpRequest, Kernels, Models, PLUGIN_LOAD, Pattern, Requests, Role, Scope,
    Secrets, Services, SessionId, Source, TOOL_CALL, ToolError as CoreToolError, Tools, Turn,
    Verdict, json,
};
use serde_json::{Value, json};
use wasmtime::StoreLimits;
use wasmtime::component::Resource;

use super::bindings::Credential;
use super::bindings::corduroy::plugin::types::{CallError, HostError, ToolError, ToolSpec};
use super::bindings::corduroy::plugin::{
    clock, events, fs_read, fs_write, http, kernel, llm, log, log_read, plugins, random, secret,
    service, state, tools, types,
};
use super::paths;
use crate::runtime::WasmRuntime;

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

/// Header names and values, in order.
type Headers = Vec<(String, String)>;

/// Largest response body `http` hands to a plugin, in bytes.
const MAX_BODY: usize = 8 << 20;

/// Most events one `log-read` call returns.
const MAX_EVENTS: u32 = 1000;

/// What the host lends a plugin's instances: the registries its capabilities reach, and the
/// plugin's `state` store. Shared by every instance of one application of the plugin.
#[derive(Clone, Default)]
pub(super) struct Lent {
    pub(super) kernels: Option<Arc<Kernels>>,
    pub(super) tools: Option<Arc<Tools>>,
    pub(super) gates: Option<Arc<Gates>>,
    pub(super) services: Option<Arc<Services>>,
    pub(super) models: Option<Arc<Models>>,
    pub(super) secrets: Option<Arc<Secrets>>,
    pub(super) http: Option<Arc<HttpClients>>,
    /// For `log-read`.
    pub(super) events: Option<Arc<EventStream>>,
    /// The `state` store as of the last call that returned.
    pub(super) state: Arc<Mutex<BTreeMap<String, String>>>,
    /// For `plugins`: where loaded plugins are compiled, how they are queued for the plugin
    /// tree, the loader's own place in it, and what it has loaded, by name.
    pub(super) loader: Option<Loader>,
}

/// What the `plugins` capability needs from the host.
#[derive(Clone)]
pub(super) struct Loader {
    pub(super) runtime: WasmRuntime,
    pub(super) requests: Requests,
    pub(super) fiber: FiberId,
    pub(super) children: Arc<Mutex<BTreeMap<String, FiberId>>>,
}

impl Loader {
    fn children(&self) -> MutexGuard<'_, BTreeMap<String, FiberId>> {
        self.children.lock().unwrap_or_else(PoisonError::into_inner)
    }
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
    /// Paths no grant reaches: the host's own files.
    pub(super) protected: Vec<PathBuf>,
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
    /// How long `llm` and `http` wait for an answer.
    pub(super) wait: Duration,
    /// The names of the credentials `secret` handed out, by handle; `None` once dropped.
    pub(super) credentials: Vec<Option<String>>,
}

impl HostState {
    fn check(&mut self, capability: &str, resource: &str) -> Result<(), HostError> {
        self.grants.check(capability, resource).map_err(|denied| {
            self.refused(capability, resource);
            HostError::Denied(denied.to_string())
        })
    }

    /// Record that `capability` was refused for `resource`.
    fn refused(&mut self, capability: &str, resource: &str) {
        let payload = format!(
            "{{\"plugin\":{},\"capability\":{},\"resource\":{}}}",
            json::string(&self.plugin),
            json::string(capability),
            json::string(resource)
        );
        self.outbox
            .push(Draft::new(Source::Host, "capability/denied").payload(payload));
    }

    /// Resolve a path named by the plugin and check it against `capability` and the host's
    /// protected paths.
    fn path(&mut self, capability: &str, path: &str) -> Result<PathBuf, HostError> {
        let resolved = paths::resolve(&self.root, path);
        let key = paths::scope_key(&resolved);
        if self
            .protected
            .iter()
            .any(|protected| resolved.starts_with(protected))
        {
            self.refused(capability, &key);
            return Err(HostError::Denied(format!(
                "`{key}` is protected by the host and out of reach of every plugin"
            )));
        }
        self.check(capability, &key)?;
        Ok(resolved)
    }
}

fn failed(path: &str, error: &std::io::Error) -> HostError {
    HostError::Failed(format!("{path}: {error}"))
}

impl types::Host for HostState {}

impl HostState {
    /// The name of the credential behind `handle`.
    fn credential(&self, handle: &Resource<Credential>) -> Option<&str> {
        let index = usize::try_from(handle.rep()).ok()?;
        self.credentials.get(index)?.as_deref()
    }
}

impl types::HostCredential for HostState {
    fn name(&mut self, self_: Resource<Credential>) -> String {
        self.credential(&self_).unwrap_or_default().to_owned()
    }

    fn drop(&mut self, rep: Resource<Credential>) -> wasmtime::Result<()> {
        if let Some(slot) = usize::try_from(rep.rep())
            .ok()
            .and_then(|index| self.credentials.get_mut(index))
        {
            *slot = None;
        }
        Ok(())
    }
}

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

/// The grant a loaded plugin gets for `capability`, asked for as `requested` by a loader
/// holding `held`: never more than the loader has.
fn narrowed(capability: &str, requested: Scope, held: &Grants) -> Result<Scope, String> {
    match held.scope(capability) {
        Some(outer) => Ok(requested.within(outer)),
        None if requested.is_empty() => Ok(Scope::none()),
        None => Err(format!(
            "cannot grant `{capability}`: the loader does not hold it, so it can grant it only \
             with an empty scope"
        )),
    }
}

impl HostState {
    /// Ask the gates whether `request` may be loaded, recording their decision. Returns why
    /// not, if not.
    fn gate_load(&mut self, request: &Value) -> Option<String> {
        let decision = self
            .lent
            .gates
            .as_ref()?
            .decide(PLUGIN_LOAD, &request.to_string())?;
        let mut recorded = request.clone();
        if let Some(fields) = recorded.as_object_mut() {
            fields.insert("point".to_owned(), json!(PLUGIN_LOAD));
            fields.insert("verdict".to_owned(), json!(decision.verdict.kind()));
            fields.insert("reason".to_owned(), json!(decision.verdict.reason()));
            fields.insert("gate".to_owned(), json!(decision.gate));
        }
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

impl HostState {
    /// The grants a plugin loaded by this one gets for `requested`, each narrowed to this
    /// plugin's own, and the request as JSON for the gates and the log.
    fn passed_on(&self, requested: Vec<plugins::Grant>) -> Result<(Grants, Value), HostError> {
        let mut grants = Grants::none();
        let mut asked = serde_json::Map::new();
        for grant in requested {
            asked.insert(
                grant.capability.clone(),
                json!({"allow": grant.allow, "deny": grant.deny}),
            );
            let scope = grant.allow.into_iter().fold(Scope::none(), Scope::allow);
            let scope = grant.deny.into_iter().fold(scope, Scope::deny);
            let scope =
                narrowed(&grant.capability, scope, &self.grants).map_err(HostError::Denied)?;
            grants = grants.grant(grant.capability, scope);
        }
        Ok((grants, Value::Object(asked)))
    }
}

impl plugins::Host for HostState {
    fn load(&mut self, request: plugins::LoadRequest) -> Result<String, HostError> {
        self.check("plugins", &request.name)?;
        let loader = self
            .lent
            .loader
            .clone()
            .ok_or_else(|| HostError::Failed(unlent("plugin loader")))?;
        if loader.children().contains_key(&request.name) {
            return Err(HostError::Failed(format!(
                "a plugin called `{}` is already loaded",
                request.name
            )));
        }
        let path = self.path("fs-read", &request.path)?;
        let plugin = loader
            .runtime
            .load(&path)
            .map_err(|error| HostError::Failed(error.to_string()))?
            .named(request.name.clone())
            .config(request.config);

        let (grants, asked) = self.passed_on(request.grants)?;
        let missing: Vec<&str> = plugin
            .requires()
            .iter()
            .map(String::as_str)
            .filter(|capability| !grants.has(capability))
            .collect();
        if !missing.is_empty() {
            return Err(HostError::Failed(format!(
                "`{}` imports {}, which the request does not grant",
                request.name,
                missing.join(", ")
            )));
        }

        let hash = plugin.hash().to_owned();
        let described = json!({
            "loader": Source::Plugin(self.plugin.clone()).to_string(),
            "name": request.name,
            "hash": hash,
            "requires": plugin.requires(),
            "grants": asked,
        });
        if let Some(refusal) = self.gate_load(&described) {
            return Err(HostError::Denied(refusal));
        }
        let id = loader
            .requests
            .load(Some(loader.fiber), plugin.with_grants(grants));
        loader.children().insert(request.name, id);
        self.outbox
            .push(Draft::new(Source::Host, "plugin/requested").payload(described.to_string()));
        Ok(hash)
    }

    fn unload(&mut self, name: String) -> Result<(), HostError> {
        self.check("plugins", &name)?;
        let loader = self
            .lent
            .loader
            .clone()
            .ok_or_else(|| HostError::Failed(unlent("plugin loader")))?;
        let id = loader
            .children()
            .remove(&name)
            .ok_or_else(|| HostError::Failed(format!("this plugin did not load `{name}`")))?;
        loader.requests.dispose(id);
        Ok(())
    }

    fn loaded(&mut self) -> Vec<String> {
        self.lent
            .loader
            .as_ref()
            .map(|loader| loader.children().keys().cloned().collect())
            .unwrap_or_default()
    }
}

impl llm::Host for HostState {
    fn complete(&mut self, model: String, request: llm::Completion) -> Result<String, HostError> {
        self.check("llm", &model)?;
        let models = self
            .lent
            .models
            .clone()
            .ok_or_else(|| HostError::Failed(unlent("models")))?;
        let completion = Completion {
            system: request.system,
            messages: request
                .messages
                .into_iter()
                .map(|turn| Turn {
                    role: match turn.role {
                        llm::Role::User => Role::User,
                        llm::Role::Assistant => Role::Assistant,
                    },
                    text: turn.text,
                })
                .collect(),
        };
        let messages: Vec<Value> = completion
            .messages
            .iter()
            .map(|turn| {
                let role = match turn.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                };
                json!({"role": role, "text": turn.text})
            })
            .collect();
        let asked = json!({"model": model, "system": completion.system, "messages": messages});
        self.outbox.push(self.draft("llm/request", &asked));
        let answer = models.complete(&model, &completion, self.wait);
        let (ok, output) = match &answer {
            Ok(text) => (true, text.clone()),
            Err(error) => (false, error.to_string()),
        };
        let answered = json!({"model": model, "ok": ok, "output": output});
        self.outbox.push(self.draft("llm/response", &answered));
        answer.map_err(|error| HostError::Failed(error.to_string()))
    }
}

impl secret::Host for HostState {
    fn get(&mut self, name: String) -> Result<Resource<Credential>, HostError> {
        self.check("secret", &name)?;
        let secrets = self
            .lent
            .secrets
            .as_ref()
            .ok_or_else(|| HostError::Failed(unlent("secrets")))?;
        if secrets.get(&name).is_none() {
            return Err(HostError::Failed(format!(
                "no secret is registered as `{name}`"
            )));
        }
        let handle = u32::try_from(self.credentials.len())
            .map_err(|_| HostError::Failed("too many credentials are open".to_owned()))?;
        self.credentials.push(Some(name));
        Ok(Resource::new_own(handle))
    }
}

/// The scheme and host of an absolute `http` or `https` URL, both in lowercase. URLs with a
/// user name or password are refused, since they make the host easy to misread.
fn authority(url: &str) -> Result<(String, String), String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("`{url}` is not an absolute URL"))?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(format!("`{url}` is not an http or https URL"));
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = rest.get(..end).unwrap_or_default();
    if authority.contains('@') {
        return Err(format!("`{url}` carries a user name or password"));
    }
    // An IPv6 address is bracketed: `[::1]:8080`.
    let host = authority
        .strip_prefix('[')
        .map_or_else(
            || authority.split(':').next(),
            |bracketed| bracketed.split_once(']').map(|(host, _)| host),
        )
        .unwrap_or_default();
    if host.is_empty() {
        return Err(format!("`{url}` names no host"));
    }
    Ok((scheme, host.to_ascii_lowercase()))
}

impl HostState {
    /// The name of the secret behind `credential`, and its value.
    fn reveal(&self, credential: &Resource<Credential>) -> Result<(String, String), HostError> {
        let name = self
            .credential(credential)
            .ok_or_else(|| HostError::Failed("the credential was dropped".to_owned()))?;
        let value = self
            .lent
            .secrets
            .as_ref()
            .ok_or_else(|| HostError::Failed(unlent("secrets")))?
            .reveal(name)
            .map_err(HostError::Failed)?;
        Ok((name.to_owned(), value))
    }

    /// `headers` with each credential's value written in, and how to record them: by name,
    /// and for a credential the secret's name, never its value.
    fn headers(&self, headers: Vec<http::Header>) -> Result<(Headers, Vec<Value>), HostError> {
        let mut written = Vec::new();
        let mut recorded = Vec::new();
        for header in headers {
            let value = match header.value {
                http::HeaderValue::Text(text) => {
                    recorded.push(json!({"name": header.name}));
                    text
                }
                http::HeaderValue::Secret(credential) => {
                    let (name, value) = self.reveal(&credential)?;
                    recorded.push(json!({"name": header.name, "secret": name}));
                    value
                }
            };
            written.push((header.name, value));
        }
        Ok((written, recorded))
    }
}

impl http::Host for HostState {
    fn send(&mut self, request: http::Request) -> Result<http::Response, HostError> {
        let (scheme, host) = authority(&request.url).map_err(HostError::Failed)?;
        self.check("http", &host)?;
        let clients = self
            .lent
            .http
            .clone()
            .ok_or_else(|| HostError::Failed(unlent("HTTP clients")))?;
        let client = clients.get(&scheme).ok_or_else(|| {
            HostError::Failed(format!("the host has no HTTP client for `{scheme}`"))
        })?;
        let (headers, recorded) = self.headers(request.headers)?;
        let body_bytes = request.body.as_ref().map_or(0, Vec::len);
        let asked = json!({
            "method": request.method, "url": request.url, "headers": recorded,
            "body": body_bytes,
        });
        self.outbox.push(self.draft("http/request", &asked));
        let sent = HttpRequest {
            method: request.method,
            url: request.url,
            headers,
            body: request.body,
        };
        let outcome = client.send(&sent, self.wait).and_then(|response| {
            if response.body.len() > MAX_BODY {
                Err(format!(
                    "the response body is over the {MAX_BODY} byte limit"
                ))
            } else {
                Ok(response)
            }
        });
        let answered = match &outcome {
            Ok(response) => json!({
                "url": sent.url, "status": response.status, "body": response.body.len(),
            }),
            Err(error) => json!({"url": sent.url, "error": error}),
        };
        self.outbox.push(self.draft("http/response", &answered));
        let response = outcome.map_err(HostError::Failed)?;
        Ok(http::Response {
            status: response.status,
            headers: response.headers,
            body: response.body,
        })
    }
}

/// An event as the plugin sees it.
pub(super) fn event_of(event: &Event) -> types::Event {
    types::Event {
        id: event.id.get(),
        session: event
            .session
            .as_ref()
            .map(|session| session.as_str().to_owned()),
        source: event.source.to_string(),
        cause: event.cause.map(EventId::get),
        topic: event.topic.clone(),
        payload: event.payload.clone(),
    }
}

impl log_read::Host for HostState {
    fn read(
        &mut self,
        session: Option<String>,
        topic: String,
        after: u64,
        limit: u32,
    ) -> Result<Vec<types::Event>, HostError> {
        let events = self
            .lent
            .events
            .clone()
            .ok_or_else(|| HostError::Failed(unlent("event stream")))?;
        let topics = Pattern::new(topic);
        let limit = usize::try_from(limit.min(MAX_EVENTS)).unwrap_or_default();
        let grants = &self.grants;
        let found = events.after(EventId::new(after), limit, |event| {
            let in_session = event.session.as_ref().map(SessionId::as_str);
            let reached = format!("{}:{}", in_session.unwrap_or_default(), event.topic);
            session
                .as_deref()
                .is_none_or(|wanted| in_session == Some(wanted))
                && topics.matches(&event.topic)
                && grants.check("log-read", &reached).is_ok()
        });
        Ok(found.iter().map(event_of).collect())
    }
}
