//! Steps of the loop that are not the harness's own.
//!
//! The loop tries, at every step, the operations the host added
//! ([`Harness::with_operation`](crate::Harness::with_operation)) and then those plugins
//! registered ([`Operations`](crate::Operations)), before any built-in step: compaction, the
//! gates, the tools and the model. The first operation that does something wins the step.
//! An operation looks at the session as it stands and says what to record; it never changes
//! the conversation itself, so everything it does is in the log.

use anyhow::Result;
use async_trait::async_trait;

use crate::session::{Effect, Session};

/// What an operation does at a step of the loop.
#[derive(Debug)]
#[non_exhaustive]
pub enum Next {
    /// Nothing: the next operation, or built-in step, is tried.
    Pass,
    /// Record these effects as one commit, and go on with the next step.
    Continue(Vec<Effect>),
    /// Record these effects as one commit, and stop the run. It ends as finished if the
    /// conversation then ends with the model's answer, and otherwise as yielded, unless a
    /// call waits for a person or a signal, in which case it waits.
    Stop(Vec<Effect>),
}

/// A step of the loop.
///
/// # Example
///
/// An operation that refuses every call to one tool before it can run:
///
/// ```
/// use corduroy_harness::{Effect, Next, Operation, Session};
/// use corduroy_harness::core::{Message, ToolResult};
///
/// struct Forbid(&'static str);
///
/// #[async_trait::async_trait]
/// impl Operation for Forbid {
///     fn name(&self) -> &str {
///         "forbid"
///     }
///
///     async fn run(&self, session: &Session) -> anyhow::Result<Next> {
///         let refused = session
///             .pending_calls()
///             .into_iter()
///             .filter(|call| call.name == self.0)
///             .fold(Message::user(), |message, call| {
///                 message.result(ToolResult::failed(call.id, "not allowed here"))
///             });
///         Ok(if refused.is_empty() {
///             Next::Pass
///         } else {
///             Next::Continue(vec![Effect::from(refused)])
///         })
///     }
/// }
/// ```
#[async_trait]
pub trait Operation: Send + Sync {
    /// The operation's name, for diagnostics and for [`Harness::step`](crate::Harness::step).
    fn name(&self) -> &str;

    /// Look at `session` and say what to do.
    ///
    /// # Errors
    ///
    /// An error is a broken pipeline: the run stops and the caller of
    /// [`Harness::run`](crate::Harness::run) gets it. A failure the model should hear of is
    /// a message, not an error.
    async fn run(&self, session: &Session) -> Result<Next>;
}
