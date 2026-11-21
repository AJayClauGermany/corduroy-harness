//! The engine plugins are compiled with, and the limits they run under.

use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use wasmtime::component::Component;
use wasmtime::{Config, Engine};
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
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            fuel_per_call: 2_000_000_000,
            memory_bytes: 64 << 20,
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
#[derive(Clone)]
pub struct WasmRuntime {
    engine: Engine,
    limits: Limits,
    root: PathBuf,
}

impl fmt::Debug for WasmRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WasmRuntime")
            .field("limits", &self.limits)
            .field("root", &self.root)
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
        let mut config = Config::new();
        config.consume_fuel(true);
        let engine = Engine::new(&config).map_err(|error| WasmError::Engine(error.to_string()))?;
        Ok(Self {
            engine,
            limits: Limits::default(),
            root: PathBuf::from("."),
        })
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
        WasmPlugin::new(self.clone(), name.into(), component)
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

/// Whether `bytes` start with the component (rather than core module) header.
///
/// Both begin with `\0asm`; the next four bytes are version 1, layer 0 for a core module and
/// version 13, layer 1 for a component.
fn wasmparser_is_component(bytes: &[u8]) -> bool {
    bytes.get(4..8) == Some(&[0x0d, 0x00, 0x01, 0x00])
}
