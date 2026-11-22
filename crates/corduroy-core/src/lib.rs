//! The dependency-free core of the corduroy harness.
//!
//! The harness is plugin-first: this crate holds the few things that are *not* plugins, and
//! they exist to give plugins somewhere to live.
//!
//! * [`context`]: the plugin tree. Everything a plugin sets up is an effect that is reverted
//!   when the plugin is unloaded, and plugins are activated and suspended as the services
//!   they depend on come and go. A plugin's name is its identity, and is its alone.
//! * [`capability`]: what a plugin may do. Plugins start with no authority and are granted
//!   scoped capabilities.
//! * [`event`]: the append-only event stream that records a run and connects plugins. Events
//!   are committed in batches to a [`Log`] before anyone sees them, carry the time they were
//!   stored, and name the session they belong to. The core defines what a log is; stores live in other crates.
//! * [`registry`], [`tool`], [`prompt`], [`kernel`], [`gate`], [`service`], [`model`]: the
//!   well-known services plugins contribute to: tools the model can call, text for the system
//!   prompt, long-lived kernels that keep state between calls, gates that decide whether
//!   something may happen, services that other plugins call across the WebAssembly boundary,
//!   and models plugins may ask for a completion of their own.
//! * [`secret`], [`http`]: credentials plugins use without seeing, and the clients the `http`
//!   capability sends through. The core checks and records; native plugins do the work.
//! * [`call`]: the tool call a thread is serving, so that what it calls in turn knows the
//!   session, and can say the call waits for a signal.
//!
//! Nothing here knows about WebAssembly or about any particular model. `corduroy-wasm` loads
//! WebAssembly components as [`Plugin`]s, and `corduroy-harness` assembles an agent loop.
//!
//! ```
//! use std::sync::Arc;
//! use corduroy_core::{App, FiberState, TOOLS, Tools, plugin_fn};
//!
//! let mut app = App::new();
//!
//! // This plugin needs the `tools` service, which nothing provides yet.
//! let greeter = app.plugin(plugin_fn("greeter", &[TOOLS], |ctx| {
//!     let _tools = ctx.service::<Tools>(TOOLS)?;
//!     Ok(())
//! }));
//! assert_eq!(app.state(greeter), Some(&FiberState::Pending));
//!
//! // Providing the service activates it.
//! app.provide(TOOLS, Arc::new(Tools::new()))?;
//! assert_eq!(app.state(greeter), Some(&FiberState::Active));
//! # Ok::<(), corduroy_core::ContextError>(())
//! ```

pub mod call;
pub mod capability;
pub mod context;
pub mod event;
pub mod gate;
pub mod http;
pub mod json;
pub mod kernel;
pub mod model;
pub mod pattern;
pub mod prompt;
pub mod registry;
pub mod secret;
pub mod service;
pub mod tool;

pub use capability::{Denied, Grants, Scope};
pub use context::{
    App, Context, ContextError, FiberId, FiberInfo, FiberState, FnPlugin, Plugin, PluginError,
    RESERVED_NAMES, Requests, plugin_fn,
};
pub use event::{Draft, Event, EventId, EventStream, ListenerId, Log, LogError, SessionId, Source};
pub use gate::{Decision, GATES, Gate, Gates, PLUGIN_LOAD, POINTS, TOOL_CALL, UNDECLARED, Verdict};
pub use http::{HTTP, HttpClient, HttpClients, HttpRequest, HttpResponse};
pub use kernel::{Cell, KERNELS, Kernel, KernelError, Kernels, MAX_OUTPUT, Process, Spawn};
pub use model::{Completion, MODELS, Model, ModelError, Models, Role, Turn};
pub use pattern::Pattern;
pub use prompt::{PROMPTS, PromptPart, Prompts};
pub use registry::{Duplicate, Registry};
pub use secret::{SECRETS, Secret, Secrets};
pub use service::{CallError, SERVICES, Service, Services};
pub use tool::{Content, FnTool, TOOLS, Tool, ToolError, ToolOutput, ToolSpec, Tools, tool_fn};
