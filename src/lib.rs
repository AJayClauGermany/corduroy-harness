//! A plugin-first agent harness.
//!
//! Almost everything here is a plugin. The harness provides a small frame (a plugin tree, an
//! event stream, the registries plugins contribute to and a loop) and plugins fill it in: the
//! model, the tools it can call, the text in its system prompt, the kernels it computes in,
//! and the reactions to what happens. Plugins are extensions (native Rust, compiled in) or
//! add-ons (WebAssembly components, plugged in without a rebuild), and add-ons hold only the
//! capabilities they were granted.
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
//! | Goose | A loop of steps that each look at the persisted conversation and return effects to store | [`harness`] |
//!
//! Runs are durable. Each step of the loop is one commit to the stream's log, a harness holds
//! many sessions on one stream, and [`Harness::continue_run`] finishes a run that a crash
//! cut short. Where the log is kept is the host's choice: any [`Log`](corduroy_core::Log), such
//! as `corduroy-jsonl`'s file.
//!
//! Gates decide whether a tool call may run. Plugins register them in the
//! [`Gates`](corduroy_core::Gates) registry; a call a gate denies goes back to the model as
//! an error, and a call a gate asks about makes the run wait, durably, until
//! [`Harness::answer`] is called.
//!
//! The loop and its types are the harness's own: [`Message`](corduroy_core::Message) is what
//! a conversation is made of, in the core, and a [`Provider`] answers a [`ModelRequest`] of
//! them. Real providers, which convert to and from each model's API, are in
//! `corduroy-providers`; [`scripted`] has one that replays a script, for tests. A run can be
//! stopped from another task with [`Harness::stop`].
//!
//! # Without the standard library
//!
//! Like the core, the harness is `no_std` and needs only an allocator; the `std` feature (on
//! by default) adds the standard library, and without it the `critical-section` feature gives
//! the locks. The loop needs no particular executor: it is plain `async`, and a stop wakes it
//! through the waker it left. The `tokio` feature (on by default) runs each tool call on
//! tokio's blocking pool when the loop runs on a tokio runtime; without it, or on another
//! executor, a tool runs where the loop does. The add-on host (`wasm`) needs the standard
//! library.
//!
//! The harness carries no extensions: kernels, log stores, HTTP clients and model providers
//! are plugins and stores of their own crates, which `corduroy-std` gathers for an ordinary
//! computer.
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
//!     let spec = ToolSpec::new("shout", "Upper-case the input.", r#"{"type":"object"}"#)
//!         .replay_safe(true);
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

#![no_std]
#![expect(
    clippy::multiple_crate_versions,
    reason = "the wasmtime dependency tree pulls several versions of shared crates"
)]

extern crate alloc;
#[cfg(any(feature = "std", test))]
extern crate std;

pub mod bound;
pub mod children;
pub mod compact;
pub mod gates;
pub mod harness;
pub mod hearing;
pub mod live;
pub mod loops;
pub mod message;
pub mod operation;
pub mod outside;
pub mod provider;
pub mod scripted;
pub mod session;
pub mod stop;
pub mod tools;

pub use children::SESSIONS;
pub use compact::CompactOperation;
pub use corduroy_core as core;
pub use corduroy_core::{ModelInfo, ModelReply, ModelRequest, StopReason, Usage};
#[cfg(feature = "wasm")]
pub use corduroy_wasm as wasm;
pub use gates::GateOperation;
pub use harness::{Harness, HarnessError, OPERATIONS, Operations, Outcome, Status, Stepped};
pub use hearing::{CHECKPOINT_TOKENS, Checkpoints};
pub use live::{Live, Seen, Shown, Watch};
pub use loops::{DEFAULT_LOOP, LOOPS, Loop, Loops, Run, StandardLoop, Step};
pub use operation::{Next, Operation};
pub use outside::{FRONTS, Front, Fronts, OUTSIDE, Outside};
pub use provider::{
    Asking, BY_PROVIDER, CallPiece, DEFAULT_PROVIDER, PROVIDERS, Piece, Provider, ProviderError,
    ProviderErrorKind, ProviderPlugin, Providers, Reports, Retry, pieces_of, provider,
};
pub use session::{
    Answer, Ask, COMPACTION, Effect, End, Journal, MAIN, MODEL_PARTIAL, MODEL_PROGRESS,
    MODEL_REQUEST, MODEL_RETRY, MODEL_SHOWN, PROCESS_START, ProcessInfo, Removal, SESSION_REMOVED,
    SIGNAL, Session, SessionInfo, Sessions, Wait, removal,
};
