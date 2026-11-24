//! The standard extensions: what the shipped `corduroy` command carries over the core and the
//! loop, for an ordinary computer running macOS, Windows or Linux.
//!
//! The core (`corduroy-core`) and the loop (`corduroy-harness`) need no operating system;
//! this crate is everything that does, put together for the systems most people have:
//!
//! | Part | From | What it is |
//! | --- | --- | --- |
//! | The loop's executor | tokio, through the harness's `tokio` feature | tool calls on a thread pool |
//! | The event log | [`jsonl`] | a JSON Lines file in the workspace |
//! | Kernels | [`shell`] | a persistent shell: POSIX (`sh`, `bash`) or PowerShell (on every Windows) |
//! | Outbound HTTP | [`http`] | the client behind the `http` capability, which never follows redirects |
//! | Models | [`providers`] | Anthropic, OpenAI-compatible, Ollama and others |
//! | MCP | [`mcp`] | MCP servers' tools, over standard input and output or HTTP |
//! | Secrets | [`extension::ENV_SECRET`] | credentials read from the environment, never handed to a plugin |
//! | Add-ons | [`wasm`] | WebAssembly plugins, each holding only the capabilities it was granted |
//!
//! [`Standard`] is that set as one value, made of parts that can each be swapped: the
//! extension kinds an agent file can ask for (kernels among them), the extensions every agent
//! gets, how the event log is stored, and whether add-ons are loaded at all. A command for
//! another kind of machine starts from [`Standard::new`] and changes the parts that do not fit,
//! or leaves this crate out and brings its own, over the same core and loop.
//!
//! ```
//! use corduroy_std::{Extensions, Standard};
//!
//! // The standard set, without add-ons and with a kernel kind of one's own.
//! let standard = Standard::new().without_addons().with_kinds(
//!     &Extensions::none().kind("sandbox-kernel", |_asked| Err("not built here".into())),
//! );
//! assert!(!standard.addons());
//! assert_eq!(standard.kinds().kernel_kinds(), ["powershell", "sandbox", "shell"]);
//! ```

#![expect(
    clippy::multiple_crate_versions,
    reason = "the wasmtime and oi-kit dependency trees each pull several versions of shared crates"
)]

pub mod extension;
pub mod places;

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use corduroy_harness::core::{EventStream, LogError, SessionId};
pub use corduroy_harness::wasm;
pub use corduroy_http as http;
pub use corduroy_jsonl as jsonl;
pub use corduroy_mcp as mcp;
pub use corduroy_providers as providers;
pub use corduroy_shell as shell;
pub use extension::{Asked, Built, ENV_SECRET, Extensions, MCP, kernel_kind};
pub use places::{DATA_DIR, Places};

/// Opens the event log kept at a path.
type OpenLog = dyn Fn(&Path) -> Result<EventStream, String> + Send + Sync;

/// Removes a session, with the sessions it started, from the event log kept at a path.
type RemoveSession = dyn Fn(&Path, &SessionId) -> Result<Vec<SessionId>, String> + Send + Sync;

/// The standard extensions, as parts that can each be swapped. See the [crate documentation](crate).
#[derive(Clone)]
pub struct Standard {
    kinds: Extensions,
    defaults: Vec<String>,
    log: Arc<OpenLog>,
    remove: Arc<RemoveSession>,
    addons: bool,
}

impl fmt::Debug for Standard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Standard")
            .field("kinds", &self.kinds)
            .field("defaults", &self.defaults)
            .field("addons", &self.addons)
            .finish_non_exhaustive()
    }
}

impl Default for Standard {
    fn default() -> Self {
        Self::new()
    }
}

impl Standard {
    /// The standard set: the [built-in kinds](Extensions::builtin), the HTTP client for
    /// every agent, the event log in a JSON Lines file, and add-ons.
    #[must_use]
    pub fn new() -> Self {
        Self {
            kinds: Extensions::builtin(),
            defaults: vec!["http-client".to_owned()],
            log: Arc::new(|path| jsonl::open(path).map_err(|error| error.to_string())),
            remove: Arc::new(|path, session| {
                let removed = jsonl::remove(path, |events| {
                    corduroy_harness::removal(events, session)
                        .map(|removal| (removal.sessions, removal.markers))
                        .map_err(|error| LogError::new(error.to_string()))
                });
                removed
                    .map(|sessions| sessions.into_iter().collect())
                    .map_err(|error| error.to_string())
            }),
            addons: true,
        }
    }

    /// Add the kinds of `kinds`, replacing those of the same name.
    #[must_use]
    pub fn with_kinds(mut self, kinds: &Extensions) -> Self {
        self.kinds = self.kinds.merged(kinds);
        self
    }

    /// Without the kind `name`, nor loading it for every agent.
    #[must_use]
    pub fn without_kind(mut self, name: &str) -> Self {
        self.kinds = self.kinds.without(name);
        self.defaults.retain(|kind| kind != name);
        self
    }

    /// Load an extension of the kind `name`, with no configuration and named like the kind,
    /// for every agent, before what its agent file asks for.
    #[must_use]
    pub fn always(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        if !self.defaults.contains(&name) {
            self.defaults.push(name);
        }
        self
    }

    /// Keep the event log with `open`, which opens (or creates) the log at a path and returns
    /// the stream over it, rather than in a JSON Lines file. Such a log cannot have sessions
    /// removed from it unless [`with_removal`](Self::with_removal) says how.
    #[must_use]
    pub fn with_log(
        mut self,
        open: impl Fn(&Path) -> Result<EventStream, String> + Send + Sync + 'static,
    ) -> Self {
        self.log = Arc::new(open);
        self.remove = Arc::new(|_, _| Err("this event log cannot remove sessions".to_owned()));
        self
    }

    /// Remove sessions from the event log with `remove`, which takes a session, and every
    /// session started from it, out of the log at a path, leaving a marker for each, and
    /// returns the sessions it removed. `corduroy_harness::removal` says which sessions and
    /// what the markers are, so that every store removes the same.
    #[must_use]
    pub fn with_removal(
        mut self,
        remove: impl Fn(&Path, &SessionId) -> Result<Vec<SessionId>, String> + Send + Sync + 'static,
    ) -> Self {
        self.remove = Arc::new(remove);
        self
    }

    /// Load no add-ons: an agent file that lists an enabled `[[plugins]]` entry is refused.
    #[must_use]
    pub const fn without_addons(mut self) -> Self {
        self.addons = false;
        self
    }

    /// The extension kinds an agent file can ask for.
    #[must_use]
    pub const fn kinds(&self) -> &Extensions {
        &self.kinds
    }

    /// The kinds loaded for every agent, in order.
    #[must_use]
    pub fn defaults(&self) -> &[String] {
        &self.defaults
    }

    /// Whether add-ons are loaded.
    #[must_use]
    pub const fn addons(&self) -> bool {
        self.addons
    }

    /// Open the event log at `path`.
    ///
    /// # Errors
    ///
    /// Why the log could not be opened.
    pub fn open_log(&self, path: &Path) -> Result<EventStream, String> {
        (self.log)(path)
    }

    /// Remove `session`, and every session started from it, from the event log at `path`,
    /// between runs. Returns the sessions removed.
    ///
    /// # Errors
    ///
    /// Why nothing was removed: the log has no such session, is open elsewhere, or cannot
    /// have sessions removed.
    pub fn remove_session(
        &self,
        path: &Path,
        session: &SessionId,
    ) -> Result<Vec<SessionId>, String> {
        (self.remove)(path, session)
    }
}
