//! The dependency-free core of the corduroy harness.
//!
//! The harness is plugin-first: this crate holds the few things that are *not* plugins, and
//! they exist to give plugins somewhere to live.
//!
//! * [`context`]: the plugin tree. Everything a plugin sets up is an effect that is reverted
//!   when the plugin is unloaded, and plugins are activated and suspended as the services
//!   they depend on come and go.
//! * [`capability`]: what a plugin may do. Plugins start with no authority and are granted
//!   scoped capabilities.
//! * [`event`]: the append-only event stream that records a run and connects plugins. Events
//!   are committed in batches to a [`Log`] before anyone sees them, and name the session
//!   they belong to. The core defines what a log is; stores live in other crates.
//! * [`registry`], [`tool`], [`prompt`], [`kernel`], [`gate`]: the well-known services
//!   plugins contribute to: tools the model can call, text for the system prompt, long-lived
//!   kernels that keep state between calls, and gates that decide whether something may
//!   happen.
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

pub mod capability;
pub mod context;
pub mod event;
pub mod gate;
pub mod json;
pub mod kernel;
pub mod pattern;
pub mod prompt;
pub mod registry;
pub mod tool;

pub use capability::{Denied, Grants, Scope};
pub use context::{
    App, Context, ContextError, FiberId, FiberInfo, FiberState, FnPlugin, Plugin, PluginError,
    plugin_fn,
};
pub use event::{Draft, Event, EventId, EventStream, ListenerId, Log, LogError, SessionId, Source};
pub use gate::{Decision, GATES, Gate, Gates, POINTS, TOOL_CALL, Verdict};
pub use kernel::{Cell, KERNELS, Kernel, KernelError, Kernels, ShellKernel};
pub use pattern::Pattern;
pub use prompt::{PROMPTS, PromptPart, Prompts};
pub use registry::{Duplicate, Registry};
pub use tool::{FnTool, TOOLS, Tool, ToolError, ToolSpec, Tools, tool_fn};
