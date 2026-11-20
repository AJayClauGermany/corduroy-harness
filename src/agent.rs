//! The loop itself.

use std::fmt;

use crate::model::{Action, Model, ModelError};
use crate::tool::ToolRegistry;
use crate::transcript::{Entry, Transcript};

/// Tunable limits for an [`Agent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Maximum number of model calls before the run is stopped.
    pub max_steps: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self { max_steps: 10 }
    }
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// The model chose [`Action::Finish`].
    Finished {
        /// The model's final answer.
        answer: String,
    },
    /// [`Config::max_steps`] was reached without the model finishing.
    StepLimitReached,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Finished { answer } => write!(f, "finished: {answer}"),
            Self::StepLimitReached => f.write_str("stopped: step limit reached"),
        }
    }
}

/// The result of a completed run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// How the run ended.
    pub status: Status,
    /// Everything that happened, in order.
    pub transcript: Transcript,
}

/// Drives a [`Model`] against a set of tools until it finishes.
#[derive(Debug)]
pub struct Agent<M> {
    model: M,
    tools: ToolRegistry,
    config: Config,
}

impl<M: Model> Agent<M> {
    /// Create an agent with the default [`Config`].
    #[must_use]
    pub fn new(model: M, tools: ToolRegistry) -> Self {
        Self {
            model,
            tools,
            config: Config::default(),
        }
    }

    /// Replace the configuration.
    #[must_use]
    pub const fn with_config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }

    /// Run the loop for `task`.
    ///
    /// Tool failures are recorded in the transcript and shown to the model, never
    /// returned from here. Hitting the step limit is reported through
    /// [`Status::StepLimitReached`], not as an error.
    ///
    /// # Errors
    ///
    /// Returns the [`ModelError`] if the model fails to produce an action. The
    /// transcript up to that point is discarded.
    pub fn run(&mut self, task: impl Into<String>) -> Result<Outcome, ModelError> {
        let mut transcript = Transcript::new(task);

        for _ in 0..self.config.max_steps {
            let action = self.model.next_action(&transcript)?;
            transcript.push(Entry::Action(action.clone()));

            match action {
                Action::Finish { answer } => {
                    return Ok(Outcome {
                        status: Status::Finished { answer },
                        transcript,
                    });
                }
                Action::CallTool { name, input } => {
                    let entry = match self.tools.call(&name, &input) {
                        Ok(output) => Entry::Output(output),
                        Err(error) => Entry::ToolError(error.to_string()),
                    };
                    transcript.push(entry);
                }
            }
        }

        Ok(Outcome {
            status: Status::StepLimitReached,
            transcript,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::{Add, Echo};
    use crate::model::ScriptedModel;

    fn call(name: &str, input: &str) -> Action {
        Action::CallTool {
            name: name.into(),
            input: input.into(),
        }
    }

    fn finish(answer: &str) -> Action {
        Action::Finish {
            answer: answer.into(),
        }
    }

    fn agent(actions: impl IntoIterator<Item = Action>) -> Agent<ScriptedModel> {
        Agent::new(
            ScriptedModel::new(actions),
            ToolRegistry::new().with(Echo).with(Add),
        )
    }

    #[test]
    fn finishes_after_a_tool_call() {
        let outcome = agent([call("add", "2 3"), finish("5")]).run("sum").unwrap();

        assert_eq!(outcome.status, Status::Finished { answer: "5".into() });
        assert_eq!(
            outcome.transcript.entries(),
            [
                Entry::Task("sum".into()),
                Entry::Action(call("add", "2 3")),
                Entry::Output("5".into()),
                Entry::Action(finish("5")),
            ]
        );
    }

    #[test]
    fn tool_errors_are_fed_back_and_the_loop_continues() {
        let outcome = agent([call("missing", ""), call("echo", "recovered"), finish("ok")])
            .run("recover")
            .unwrap();

        assert_eq!(
            outcome.status,
            Status::Finished {
                answer: "ok".into()
            }
        );
        assert!(
            outcome
                .transcript
                .entries()
                .contains(&Entry::ToolError("unknown tool `missing`".into()))
        );
        assert!(
            outcome
                .transcript
                .entries()
                .contains(&Entry::Output("recovered".into()))
        );
    }

    #[test]
    fn stops_at_the_step_limit() {
        let mut agent =
            agent(std::iter::repeat_n(call("echo", "x"), 100)).with_config(Config { max_steps: 3 });

        let outcome = agent.run("loop forever").unwrap();

        assert_eq!(outcome.status, Status::StepLimitReached);
        assert_eq!(outcome.transcript.action_count(), 3);
    }

    #[test]
    fn zero_steps_never_consults_the_model() {
        let mut agent = agent([]).with_config(Config { max_steps: 0 });

        let outcome = agent.run("nothing").unwrap();

        assert_eq!(outcome.status, Status::StepLimitReached);
        assert_eq!(outcome.transcript.action_count(), 0);
    }

    #[test]
    fn model_errors_abort_the_run() {
        let result = agent([call("echo", "x")]).run("runs dry");

        assert_eq!(result, Err(ModelError::ScriptExhausted));
    }

    #[test]
    fn status_display_is_human_readable() {
        assert_eq!(
            Status::Finished {
                answer: "42".into()
            }
            .to_string(),
            "finished: 42"
        );
        assert_eq!(
            Status::StepLimitReached.to_string(),
            "stopped: step limit reached"
        );
    }
}
