//! Asking the gates before a tool call runs.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec::Vec;

use anyhow::Result;
use async_trait::async_trait;
use corduroy_core::call::Serve;
use corduroy_core::{Decision, Gates, Message, TOOL_CALL, ToolCall, ToolResult, Tools, Verdict};
use serde_json::{Value, json};

use crate::operation::{Next, Operation};
use crate::session::{Answer, Effect, Gated, Session, Sessions};

/// The step that asks the gates about each tool call before the tools step may run it.
///
/// For every call the model asked for and that has no response yet, the gates registered
/// for [`TOOL_CALL`] are asked once, in the call's session scope, and their decision is
/// stored as a `gate/decision`, in the same commit as anything else the step does. Then:
///
/// - An allowed call is left to the tools step.
/// - A denied call is answered with an error result carrying the gate's reason, so the
///   model can try something else.
/// - A call a gate asked about waits for [`Harness::answer`]. While any call waits, the
///   step stops the run and no call of the message runs; the run is *waiting*.
///
/// With no gate registered, or none with an opinion, nothing is recorded and every call
/// runs as before.
///
/// [`Harness::answer`]: crate::Harness::answer
#[derive(Debug)]
pub struct GateOperation {
    gates: Arc<Gates>,
    tools: Arc<Tools>,
    sessions: Arc<Sessions>,
}

impl GateOperation {
    /// Ask the gates in `gates` about calls to the tools in `tools`, recording decisions in
    /// each session's journal in `sessions`.
    #[must_use]
    pub const fn new(gates: Arc<Gates>, tools: Arc<Tools>, sessions: Arc<Sessions>) -> Self {
        Self {
            gates,
            tools,
            sessions,
        }
    }

    /// Ask the gates about `call` of `session`, or `None` if none has an opinion.
    fn decide(&self, session: &Session, call: &ToolCall) -> Option<Decision> {
        let read_only = self
            .tools
            .get(&call.name)
            .is_some_and(|tool| tool.spec().read_only);
        let input: Value = serde_json::from_str(&call.input).unwrap_or_else(|_| json!({}));
        let request = json!({
            "session": session.id().as_str(),
            "id": call.id,
            "tool": call.name,
            "input": input,
            "read-only": read_only,
            "caller": "agent",
        });
        let serve = Serve::session(session.id().clone());
        serve
            .run(|| self.gates.decide(TOOL_CALL, &request.to_string()))
            .0
    }
}

#[async_trait]
impl Operation for GateOperation {
    fn name(&self) -> &'static str {
        "gates"
    }

    async fn run(&self, session: &Session) -> Result<Next> {
        let journal = self.sessions.journal(session.id())?;
        let mut effects = Vec::new();
        let mut refusals = Message::user();
        let mut waiting = false;
        for call in session.pending_calls() {
            let gated = if let Some(gated) = journal.gated(&call.id) {
                gated
            } else {
                let Some(decision) = self.decide(session, &call) else {
                    continue;
                };
                let gated = Gated {
                    verdict: decision.verdict.clone(),
                    gate: decision.gate.clone(),
                    answer: None,
                };
                effects.push(Effect::Gated {
                    call: call.id.clone(),
                    decision,
                });
                gated
            };
            match (gated.verdict, gated.answer) {
                (Verdict::Allow, _) | (Verdict::Ask(_), Some(Answer::Allow)) => {}
                (Verdict::Ask(_), None) => waiting = true,
                (Verdict::Ask(_), Some(Answer::Deny(reason))) => {
                    // Whoever answers a gate is the person the agent works for: said so, so
                    // that a model takes the reason as their word and not a stranger's.
                    let reason =
                        format!("the person you are working for declined this call: {reason}");
                    refusals = refusals.result(ToolResult::failed(call.id, reason));
                }
                (Verdict::Deny(reason), _) => {
                    let reason = format!("denied by gate `{}`: {reason}", gated.gate);
                    refusals = refusals.result(ToolResult::failed(call.id, reason));
                }
            }
        }
        if !refusals.is_empty() {
            effects.push(Effect::from(refusals));
        }
        Ok(if waiting {
            Next::Stop(effects)
        } else if effects.is_empty() {
            Next::Pass
        } else {
            Next::Continue(effects)
        })
    }
}
