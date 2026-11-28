//! The engine plugins are compiled with, and the limits they run under.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Write as _};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use sha2::{Digest as _, Sha256};
use wasmtime::component::Component;
use wasmtime::{Cache, CacheConfig, Config, Engine};
use wit_component::ComponentEncoder;

use crate::plugin::WasmPlugin;

/// Resource limits applied to every plugin instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Fuel available to each call into a plugin. One unit is roughly one WebAssembly
    /// instruction; a call that runs out traps instead of spinning forever.
    pub fuel_per_call: u64,
    /// Most linear memory an instance may grow to, in bytes.
    pub memory_bytes: usize,
    /// How many times a plugin is started again after a call traps. The trap after the last
    /// restart stops it until it is reloaded. Counted from when the plugin was applied.
    pub restarts: u32,
    /// How long a call made from inside another plugin's call waits for the plugin it calls
    /// to finish a call on another thread. Bounding it means two chains of calls that wait on
    /// each other give up instead of deadlocking.
    pub nested_wait: Duration,
    /// How long a chain of causes may lead up to an event a plugin is called for. Plugins
    /// that react to each other's events (automations, say) are stopped at this depth rather
    /// than going on forever; `plugin/cascade` records where.
    pub cascade: usize,
    /// How long `llm` and `http` wait for an answer before the call fails.
    pub outbound_wait: Duration,
    /// How many events are kept for a plugin while it is in a call, to be handed over when
    /// the call returns. One more and the oldest goes; `plugin/missed` records which.
    pub missed_events: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            fuel_per_call: 2_000_000_000,
            memory_bytes: 64 << 20,
            restarts: 3,
            nested_wait: Duration::from_secs(30),
            cascade: 16,
            outbound_wait: Duration::from_secs(120),
            missed_events: 1024,
        }
    }
}

/// Why a plugin could not be loaded.
#[derive(Debug)]
pub enum WasmError {
    /// The engine could not be created.
    Engine(String),
    /// The plugin file could not be read.
    Io {
        /// The file that was being read.
        path: PathBuf,
        /// What went wrong.
        reason: String,
    },
    /// The bytes are not a WebAssembly component, nor a core module that can be made into
    /// one.
    Invalid(String),
}

impl fmt::Display for WasmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Engine(reason) => write!(f, "cannot start the WebAssembly engine: {reason}"),
            Self::Io { path, reason } => write!(f, "cannot read `{}`: {reason}", path.display()),
            Self::Invalid(reason) => write!(f, "not a valid plugin: {reason}"),
        }
    }
}

impl Error for WasmError {}

/// Compiles WebAssembly plugins. Cheap to clone; clones share compiled code caches.
///
/// A component is compiled once per runtime and kept by the SHA-256 of its bytes, so loading
/// the same plugin twice (a script host for two automations, say) compiles it once. With
/// [`cached`](Self::cached), compiled code is also kept on disk for later processes.
///
/// Some paths are *protected*: no plugin loaded through this runtime can read or write them
/// through `fs-read` or `fs-write`, whatever its grant says. The host binary and the disk
/// cache are protected from the start; the host adds the rest (the event log, grant files)
/// with [`protect`](Self::protect).
#[derive(Clone)]
pub struct WasmRuntime {
    engine: Engine,
    limits: Limits,
    root: PathBuf,
    compiled: Arc<Mutex<BTreeMap<String, Component>>>,
    protected: Arc<[PathBuf]>,
}

impl fmt::Debug for WasmRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WasmRuntime")
            .field("limits", &self.limits)
            .field("root", &self.root)
            .field("protected", &self.protected)
            .finish_non_exhaustive()
    }
}

impl WasmRuntime {
    /// A runtime with the default [`Limits`], resolving relative plugin paths against the
    /// current directory.
    ///
    /// # Errors
    ///
    /// Returns [`WasmError::Engine`] if the engine cannot be configured on this machine.
    pub fn new() -> Result<Self, WasmError> {
        Self::with_config(Config::new())
    }

    /// A runtime that also keeps compiled code in `dir`, so that a later process loading the
    /// same component skips compiling it. The directory holds machine code the host will run,
    /// so it is protected from plugins. It is the host's to choose; nothing is cached on disk
    /// unless asked.
    ///
    /// # Errors
    ///
    /// Returns [`WasmError::Engine`] if the directory cannot be used or the engine cannot be
    /// configured.
    pub fn cached(dir: impl Into<PathBuf>) -> Result<Self, WasmError> {
        let dir = dir.into();
        fs::create_dir_all(&dir).map_err(|error| WasmError::Engine(error.to_string()))?;
        let mut cache = CacheConfig::new();
        cache.with_directory(&dir);
        let cache = Cache::new(cache).map_err(|error| WasmError::Engine(format!("{error:#}")))?;
        let mut config = Config::new();
        config.cache(Some(cache));
        Ok(Self::with_config(config)?.protect(dir))
    }

    fn with_config(mut config: Config) -> Result<Self, WasmError> {
        config.consume_fuel(true);
        let engine = Engine::new(&config).map_err(|error| WasmError::Engine(error.to_string()))?;
        let runtime = Self {
            engine,
            limits: Limits::default(),
            root: PathBuf::from("."),
            compiled: Arc::default(),
            protected: Arc::default(),
        };
        // A plugin must not be able to replace the program that enforces its grants.
        Ok(match std::env::current_exe() {
            Ok(host) => runtime.protect(host),
            Err(_) => runtime,
        })
    }

    /// Put `path`, and everything below it, out of reach of every plugin loaded from now on.
    #[must_use]
    pub fn protect(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let path = path.canonicalize().unwrap_or(path);
        let mut protected = self.protected.to_vec();
        protected.push(path);
        self.protected = protected.into();
        self
    }

    /// How many distinct components this runtime, and its clones, have compiled.
    #[must_use]
    pub fn components(&self) -> usize {
        self.compiled
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// The paths no plugin may read or write.
    #[must_use]
    pub fn protected(&self) -> &[PathBuf] {
        &self.protected
    }

    /// Replace the limits applied to plugins loaded from now on.
    #[must_use]
    pub const fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Resolve the relative paths plugins pass to `fs-read` and `fs-write` against `root`
    /// instead of the current directory.
    #[must_use]
    pub fn with_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = root.into();
        self
    }

    /// Load a plugin from a `.wasm` file. The plugin has no capabilities until they are
    /// granted with [`WasmPlugin::grant`].
    ///
    /// # Errors
    ///
    /// Returns [`WasmError::Io`] if the file cannot be read, or [`WasmError::Invalid`] if it
    /// is not a plugin.
    pub fn load(&self, path: impl AsRef<Path>) -> Result<WasmPlugin, WasmError> {
        let path = path.as_ref();
        let bytes = fs::read(path).map_err(|error| WasmError::Io {
            path: path.to_owned(),
            reason: error.to_string(),
        })?;
        let name = path.file_stem().map_or_else(
            || "plugin".into(),
            |stem| stem.to_string_lossy().into_owned(),
        );
        self.load_bytes(name, &bytes)
    }

    /// Load a plugin from bytes already in memory. `name` is used until the plugin reports
    /// its own.
    ///
    /// Accepts a component, or a core module carrying component type information, which is
    /// what `cargo build --target wasm32-unknown-unknown` produces for a crate using
    /// `wit-bindgen`. A core module is wrapped into a component first.
    ///
    /// # Errors
    ///
    /// Returns [`WasmError::Invalid`] if the bytes are neither.
    pub fn load_bytes(
        &self,
        name: impl Into<String>,
        bytes: &[u8],
    ) -> Result<WasmPlugin, WasmError> {
        let hash = hash(bytes);
        let known = self
            .compiled
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&hash)
            .cloned();
        if let Some(component) = known {
            return WasmPlugin::new(self.clone(), name.into(), component, hash);
        }
        let invalid = |error: wasmtime::Error| WasmError::Invalid(format!("{error:#}"));
        let component = if wasmparser_is_component(bytes) {
            Component::from_binary(&self.engine, bytes).map_err(invalid)?
        } else {
            let wrapped = ComponentEncoder::default()
                .validate(true)
                .module(bytes)
                .and_then(ComponentEncoder::encode)
                .map_err(|error| WasmError::Invalid(format!("{error:#}")))?;
            Component::from_binary(&self.engine, &wrapped).map_err(invalid)?
        };
        self.compiled
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(hash.clone(), component.clone());
        WasmPlugin::new(self.clone(), name.into(), component, hash)
    }

    pub(crate) const fn engine(&self) -> &Engine {
        &self.engine
    }

    pub(crate) const fn limits(&self) -> Limits {
        self.limits
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
}

/// The SHA-256 of `bytes`, in lowercase hex.
pub(crate) fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            // Writing to a `String` cannot fail.
            write!(hex, "{byte:02x}").unwrap_or_default();
            hex
        })
}

/// Whether `bytes` start with the component (rather than core module) header.
///
/// Both begin with `\0asm`; the next four bytes are version 1, layer 0 for a core module and
/// version 13, layer 1 for a component.
fn wasmparser_is_component(bytes: &[u8]) -> bool {
    bytes.get(4..8) == Some(&[0x0d, 0x00, 0x01, 0x00])
}
