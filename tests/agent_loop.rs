//! End-to-end behaviour of the loop through the public API only.

use corduroy_harness::{
    Action, Add, Agent, Config, Echo, Entry, ModelError, Status, ToolRegistry, Transcript,
};

/// A reactive model: tries a bad tool first, reads the error, then recovers.
fn recovering_model(transcript: &Transcript) -> Result<Action, ModelError> {
    match transcript.last() {
        Some(Entry::Task(_)) => Ok(Action::CallTool {
            name: "calculator".into(),
            input: "2 3 4".into(),
        }),
        Some(Entry::ToolError(_)) => Ok(Action::CallTool {
            name: "add".into(),
            input: "2 3 4".into(),
        }),
        Some(Entry::Output(sum)) => Ok(Action::Finish {
            answer: sum.clone(),
        }),
        Some(Entry::Action(_)) | None => Err(ModelError::Failed("unexpected state".into())),
    }
}

#[test]
fn model_reacts_to_tool_errors_and_recovers() {
    let mut agent = Agent::new(recovering_model, ToolRegistry::new().with(Add));

    let outcome = agent.run("add 2 3 4").unwrap();

    assert_eq!(outcome.status, Status::Finished { answer: "9".into() });
    assert_eq!(outcome.transcript.action_count(), 3);
}

#[test]
fn a_model_that_never_finishes_is_cut_off() {
    let always_echo = |_: &Transcript| {
        Ok(Action::CallTool {
            name: "echo".into(),
            input: "again".into(),
        })
    };
    let mut agent = Agent::new(always_echo, ToolRegistry::new().with(Echo))
        .with_config(Config { max_steps: 5 });

    let outcome = agent.run("spin").unwrap();

    assert_eq!(outcome.status, Status::StepLimitReached);
    assert_eq!(outcome.transcript.action_count(), 5);
}
