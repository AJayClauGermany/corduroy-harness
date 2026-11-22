//! Sub-agents, after Goose's summon: hand a task to an agent of its own and wait for its
//! answer.
//!
//! `delegate` asks the harness, through its `sessions` service, to start a child session for
//! the task. The child is a run of its own (the same model, tools and gates, an empty
//! conversation but for the task), so the work it does stays out of the parent's context and
//! only its answer comes back. The parent's run waits for it in the log: the harness runs the
//! child, and its final answer becomes the result of the `delegate` call.
//!
//! Needs the `service` capability scoped to `sessions`, and nothing else.

use corduroy_guest::host::service;
use corduroy_guest::{CallError, Plugin, PluginInfo, ToolError, ToolSpec, parse};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct Delegate {
    task: String,
    instructions: Option<String>,
}

fn delegate(input: &Delegate) -> Result<String, ToolError> {
    if input.task.trim().is_empty() {
        return Err(ToolError::InvalidInput("the task is empty".to_owned()));
    }
    let payload = json!({"task": input.task, "instructions": input.instructions});
    let started =
        service::call("sessions", "start", &payload.to_string()).map_err(|error| match error {
            CallError::Denied(reason) => ToolError::Denied(reason),
            CallError::InvalidInput(reason) => ToolError::InvalidInput(reason),
            CallError::Unknown(reason) | CallError::Failed(reason) => ToolError::Failed(reason),
        })?;
    // The harness puts the sub-agent's answer in place of this once it has one.
    Ok(format!("Started a sub-agent: {started}"))
}

struct Subagents;

impl Plugin for Subagents {
    fn activate(_config: String) -> Result<PluginInfo, String> {
        Ok(PluginInfo::new("subagents", env!("CARGO_PKG_VERSION")).tool(ToolSpec::new(
            "delegate",
            "Hand a self-contained task to a sub-agent and get back its answer. The sub-agent \
             has the same tools but none of this conversation, so give it everything it needs \
             in the task. Use it for work that would fill this conversation with detail you \
             do not need afterwards: searching, reading many files, trying things out.",
            r#"{"type":"object","properties":{"task":{"type":"string","description":"What to do, and what to report back"},"instructions":{"type":"string","description":"How to go about it, if it matters"}},"required":["task"]}"#,
        )))
    }

    fn call_tool(name: String, input: String) -> Result<String, ToolError> {
        match name.as_str() {
            "delegate" => delegate(&parse(&input)?),
            _ => Err(ToolError::UnknownTool(name)),
        }
    }
}

corduroy_guest::export!(Subagents);
