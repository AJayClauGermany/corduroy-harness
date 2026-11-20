//! Demo: a tiny rule-based "model" adds some numbers using the `add` tool.
//!
//! Run with `cargo demo`. No network or LLM involved.

use std::error::Error;
use std::io::{self, Write};

use corduroy_harness::{Action, Add, Agent, Echo, Entry, ModelError, ToolRegistry, Transcript};

/// Decides what to do by looking only at the most recent transcript entry.
fn demo_model(transcript: &Transcript) -> Result<Action, ModelError> {
    match transcript.last() {
        Some(Entry::Task(_)) => Ok(Action::CallTool {
            name: "add".to_owned(),
            input: "2 3 4".to_owned(),
        }),
        Some(Entry::Output(sum)) => Ok(Action::Finish {
            answer: format!("the total is {sum}"),
        }),
        Some(Entry::ToolError(error)) => Ok(Action::Finish {
            answer: format!("giving up: {error}"),
        }),
        Some(Entry::Action(_)) | None => Err(ModelError::Failed(
            "transcript ended on an action".to_owned(),
        )),
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let tools = ToolRegistry::new().with(Echo).with(Add);
    let mut agent = Agent::new(demo_model, tools);

    let outcome = agent.run("Add 2, 3 and 4")?;

    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{}", outcome.transcript)?;
    writeln!(stdout, "--\n{}", outcome.status)?;
    Ok(())
}
