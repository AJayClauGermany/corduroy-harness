//! The decision-making half of the loop.

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;

use crate::transcript::Transcript;

/// What a [`Model`] wants to happen next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Invoke the named tool with a free-form input string.
    CallTool {
        /// Name the tool is registered under.
        name: String,
        /// Input passed to the tool verbatim.
        input: String,
    },
    /// Stop the loop and report `answer` as the final result.
    Finish {
        /// The final answer.
        answer: String,
    },
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CallTool { name, input } => write!(f, "call {name}({input})"),
            Self::Finish { answer } => write!(f, "finish ({answer})"),
        }
    }
}

/// Why a [`Model`] could not propose an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    /// A [`ScriptedModel`] was asked for more actions than it was given.
    ScriptExhausted,
    /// Any other failure, with a human-readable description.
    Failed(String),
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ScriptExhausted => f.write_str("scripted model ran out of actions"),
            Self::Failed(reason) => write!(f, "model failed: {reason}"),
        }
    }
}

impl Error for ModelError {}

/// Chooses the next [`Action`] given the transcript so far.
///
/// Any `FnMut(&Transcript) -> Result<Action, ModelError>` is a `Model`, which makes
/// small rule-based models in tests a one-liner.
pub trait Model {
    /// Propose the next action.
    ///
    /// # Errors
    ///
    /// Returns a [`ModelError`] if no action could be produced. The run is aborted.
    fn next_action(&mut self, transcript: &Transcript) -> Result<Action, ModelError>;
}

impl<F> Model for F
where
    F: FnMut(&Transcript) -> Result<Action, ModelError>,
{
    fn next_action(&mut self, transcript: &Transcript) -> Result<Action, ModelError> {
        self(transcript)
    }
}

/// A model that replays a fixed list of actions, ignoring the transcript.
#[derive(Debug, Clone)]
pub struct ScriptedModel {
    remaining: VecDeque<Action>,
}

impl ScriptedModel {
    /// Create a model that will return `actions` in order.
    #[must_use]
    pub fn new(actions: impl IntoIterator<Item = Action>) -> Self {
        Self {
            remaining: actions.into_iter().collect(),
        }
    }
}

impl Model for ScriptedModel {
    fn next_action(&mut self, _transcript: &Transcript) -> Result<Action, ModelError> {
        self.remaining
            .pop_front()
            .ok_or(ModelError::ScriptExhausted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finish(answer: &str) -> Action {
        Action::Finish {
            answer: answer.into(),
        }
    }

    #[test]
    fn scripted_model_replays_in_order_then_errors() {
        let mut model = ScriptedModel::new([finish("a"), finish("b")]);
        let transcript = Transcript::new("task");

        assert_eq!(model.next_action(&transcript), Ok(finish("a")));
        assert_eq!(model.next_action(&transcript), Ok(finish("b")));
        assert_eq!(
            model.next_action(&transcript),
            Err(ModelError::ScriptExhausted)
        );
    }

    #[test]
    fn closures_are_models() {
        let mut calls = 0_u32;
        let mut model = |_: &Transcript| {
            calls += 1;
            Ok(finish("ok"))
        };

        assert_eq!(
            model.next_action(&Transcript::new("task")),
            Ok(finish("ok"))
        );
        assert_eq!(calls, 1);
    }

    #[test]
    fn display_is_human_readable() {
        let call = Action::CallTool {
            name: "add".into(),
            input: "1 2".into(),
        };

        assert_eq!(call.to_string(), "call add(1 2)");
        assert_eq!(finish("x").to_string(), "finish (x)");
        assert_eq!(
            ModelError::Failed("boom".into()).to_string(),
            "model failed: boom"
        );
    }
}
