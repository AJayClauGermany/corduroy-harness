//! A minimal agent harness.
//!
//! The harness runs a simple loop:
//!
//! 1. Ask the [`Model`] for the next [`Action`], given everything seen so far.
//! 2. If the action is [`Action::CallTool`], run the tool and record its output
//!    (or its error) in the [`Transcript`]. Tool errors are fed back to the model
//!    rather than aborting the run, so it can recover.
//! 3. If the action is [`Action::Finish`], stop. Otherwise repeat, up to
//!    [`Config::max_steps`].
//!
//! The model is a trait, so tests can drive the loop with a [`ScriptedModel`] or a
//! plain closure and no network or LLM is needed.
//!
//! ```
//! use corduroy_harness::{Action, Agent, Echo, ScriptedModel, Status, ToolRegistry};
//!
//! let model = ScriptedModel::new([
//!     Action::CallTool { name: "echo".into(), input: "hi".into() },
//!     Action::Finish { answer: "done".into() },
//! ]);
//! let mut agent = Agent::new(model, ToolRegistry::new().with(Echo));
//!
//! let outcome = agent.run("say hi")?;
//! assert_eq!(outcome.status, Status::Finished { answer: "done".into() });
//! # Ok::<(), corduroy_harness::ModelError>(())
//! ```

pub mod agent;
pub mod builtin;
pub mod model;
pub mod tool;
pub mod transcript;

pub use agent::{Agent, Config, Outcome, Status};
pub use builtin::{Add, Echo};
pub use model::{Action, Model, ModelError, ScriptedModel};
pub use tool::{Tool, ToolError, ToolRegistry};
pub use transcript::{Entry, Transcript};
