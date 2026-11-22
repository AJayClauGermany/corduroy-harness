//! The host side of each capability: what actually happens when a plugin calls an import.
//!
//! Every function here follows the same shape: work out which resource the plugin is asking
//! for, check it against the plugin's grants, then do the work. A refusal is reported to the
//! plugin as `denied` and recorded on the event stream as `capability/denied`.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use corduroy_core::{Draft, Grants, Kernels, Source, json};
use wasmtime::StoreLimits;

use super::bindings::corduroy::plugin::types::HostError;
use super::bindings::corduroy::plugin::{
    clock, events, fs_read, fs_write, kernel, log, random, types,
};
use super::paths;

/// Largest file `fs-read` will hand to a plugin, in bytes.
const MAX_FILE: u64 = 8 << 20;

/// Longest message `log` records, in bytes. Longer ones are cut short.
const MAX_LOG: usize = 16 << 10;

/// Most bytes `random` hands out per call.
const MAX_RANDOM: u32 = 64 << 10;

/// Everything the host knows about one plugin instance. Lives in the instance's store.
pub(super) struct HostState {
    /// Name used as the source of the plugin's events.
    pub(super) plugin: String,
    pub(super) grants: Grants,
    /// Directory relative paths are resolved against.
    pub(super) root: PathBuf,
    pub(super) kernels: Option<Arc<Kernels>>,
    /// Events raised during the current call, delivered once it returns. Delivering them at
    /// once could re-enter the instance that is still running.
    pub(super) outbox: Vec<Draft>,
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
