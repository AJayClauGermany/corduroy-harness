//! A plugin-first agent harness.
//!
//! Almost everything here is a plugin. The harness provides a small frame (a plugin tree, an
//! event stream, the registries plugins contribute to and a loop) and plugins fill it in: the
//! model, the tools it can call, the text in its system prompt, the kernels it computes in,
//! and the reactions to what happens. Plugins can be native Rust or WebAssembly components, and WebAssembly
//! plugins hold only the capabilities they were granted.
//!
//! The design borrows one idea from each of six harnesses:
//!
//! | From | Idea | Where |
//! | --- | --- | --- |
//! | Cordis | Plugins whose effects revert on unload and which follow their dependencies | [`corduroy_core::context`] |
//! | Prime Intellect | Long-lived kernels that keep state between calls | [`corduroy_core::kernel`] |
//! | Hermes | Skills the agent writes and repairs for itself | the `skills` plugin |
//! | Pi | A minimal toolset: read, write, edit, bash | the `pi-tools` plugin |
//! | `OpenHands` | An append-only event stream as the source of truth | [`corduroy_core::event`], [`session`] |
//! | Goose | The base abstractions, from oi-kit | [`harness`] |
//!
//! Runs are durable. Each step of the loop is one commit to the stream's log, a harness holds
//! many sessions on one stream, and [`Harness::continue_run`] finishes a run that a crash
//! cut short. [`jsonl`] keeps the log in a file.
//!
//! Gates decide whether a tool call may run. Plugins register them in the
//! [`Gates`](corduroy_core::Gates) registry; a call a gate denies goes back to the model as
//! an error, and a call a gate asks about makes the run wait, durably, until
//! [`Harness::answer`] is called.
//!
//! oi-kit supplies the provider, message and conversation types and the state machine that
//! runs the loop; this crate re-exports them as [`oi_kit_agent`] and [`oi_kit_provider_types`].
//!
//! ```
//! use std::sync::Arc;
//! use corduroy_harness::scripted::{ScriptedProvider, call, say};
//! use corduroy_harness::{Harness, Status};
//! use corduroy_harness::core::{TOOLS, ToolSpec, Tools, plugin_fn, tool_fn};
//! use serde_json::json;
//!
//! # tokio::runtime::Runtime::new()?.block_on(async {
//! // A model that calls one tool and then answers. No network, no LLM.
//! let model = ScriptedProvider::new([
//!     call("call-1", "shout", &json!({"text": "hi"})),
//!     say("done"),
//! ]);
//! let mut harness = Harness::new(Arc::new(model))?;
//!
//! // A native plugin that contributes the tool. Unloading it would take the tool away.
//! harness.plugin(plugin_fn("shouter", &[TOOLS], |ctx| {
//!     let spec = ToolSpec {
//!         name: "shout".into(),
//!         description: "Upper-case the input.".into(),
//!         input_schema: r#"{"type":"object"}"#.into(),
//!         replay_safe: true,
//!         read_only: false,
//!     };
//!     let tools = ctx.service::<Tools>(TOOLS)?;
//!     tools.register(ctx, "shout", Arc::new(tool_fn(spec, |input| Ok(input.to_uppercase()))))?;
//!     Ok(())
//! }));
//!
//! let outcome = harness.run("say hi loudly").await?;
//! assert_eq!(outcome.status, Status::Finished { answer: "done".into() });
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! # })?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![expect(
    clippy::multiple_crate_versions,
    reason = "the wasmtime and oi-kit dependency trees each pull several versions of shared crates"
)]

pub mod gates;
pub mod harness;
pub mod provider;
pub mod scripted;
pub mod session;
pub mod tools;

pub use corduroy_core as core;
pub use corduroy_jsonl as jsonl;
pub use corduroy_shell as shell;
pub use corduroy_shell::shell_kernel;
pub use corduroy_wasm as wasm;
pub use gates::GateOperation;
pub use harness::{Harness, HarnessError, Outcome, Status};
pub use provider::{DEFAULT_PROVIDER, PROVIDERS, ProviderPlugin, Providers, provider};
pub use session::{Answer, Ask, Effect, Journal, MAIN, Session, SessionInfo, Sessions};
pub use {oi_kit_agent, oi_kit_provider_types};
