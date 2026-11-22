//! Asking the gates before a tool call runs.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use std::sync::{Mutex, PoisonError};

use corduroy_core::{App, Decision, Gates, SessionId, TOOL_CALL, Tools, Verdict};
use oi_kit_agent::machine::MachineSession as _;
use oi_kit_agent::operation::{
    Emitter, Operation, OperationResult, applied, messages_since_kickoff, not_applicable,
    yielded_with,
};
use oi_kit_agent::tool_call_state::pending_tool_requests;
use oi_kit_provider_types::conversation::Conversation;
use oi_kit_provider_types::conversation::message::Message;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use serde_json::json;

use crate::session::{Answer, Effect, Gated, Session, Sessions};

/// The step that asks the gates about each tool call before the tools step may run it.
///
/// For every call the model asked for and that has no response yet, the gates registered
/// for [`TOOL_CALL`] are asked once and their decision is stored as a `gate/decision`, in
/// the same commit as anything else the step does. Then:
///
/// - An allowed call is left to the tools step.
/// - A denied call is answered with an error result carrying the gate's reason, so the
///   model can try something else.
/// - A call a gate asked about waits for [`Harness::answer`]. While any call waits, the
///   step yields and no call of the message runs; the run is *waiting*.
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

    /// Ask the gates about the call `id` of `session`, or `None` if none has an opinion.
    fn decide(&self, session: &str, id: &str, call: &CallToolRequestParams) -> Option<Decision> {
        let read_only = self
            .tools
            .get(&call.name)
            .is_some_and(|tool| tool.spec().read_only);
        let request = json!({
            "session": session,
            "id": id,
            "tool": call.name,
            "input": call.arguments.clone().unwrap_or_default(),
            "read-only": read_only,
            "caller": "agent",
        });
        self.gates.decide(TOOL_CALL, &request.to_string())
    }
}

/// An error result for `call` telling the model why it did not run.
fn refuse(message: Message, call: &str, reason: String) -> Message {
    let result = CallToolResult::error(vec![ContentBlock::text(reason)]);
    message.with_tool_response(call.to_owned(), Ok(result))
}

#[async_trait]
impl Operation<Session, Effect> for GateOperation {
    fn name(&self) -> &'static str {
        "gates"
    }

    async fn run(
        &self,
        session: &Session,
        conversation: &Conversation,
        _emit: &Emitter,
    ) -> Result<OperationResult<Effect>> {
        let Ok(turn) = messages_since_kickoff(conversation) else {
            return not_applicable();
        };
        let journal = self.sessions.journal(&SessionId::new(session.id()))?;
        let mut effects = Vec::new();
        let mut refusals = Message::user();
        let mut waiting = false;
        for request in pending_tool_requests(turn) {
            // A call that could not be decoded is refused by the tools step.
            let Ok(call) = request.tool_call.as_ref() else {
                continue;
            };
            let id = request.id.as_str();
            let gated = if let Some(gated) = journal.gated(id) {
                gated
            } else {
                let Some(decision) = self.decide(session.id(), id, call) else {
                    continue;
                };
                let gated = Gated {
                    verdict: decision.verdict.clone(),
                    gate: decision.gate.clone(),
                    answer: None,
                };
                effects.push(Effect::Gated {
                    call: id.to_owned(),
                    decision,
                });
                gated
            };
            match (gated.verdict, gated.answer) {
                (Verdict::Allow, _) | (Verdict::Ask(_), Some(Answer::Allow)) => {}
                (Verdict::Ask(_), None) => waiting = true,
                (Verdict::Ask(_), Some(Answer::Deny(reason))) => {
                    let reason = format!("a person declined this call: {reason}");
                    refusals = refuse(refusals, id, reason);
                }
                (Verdict::Deny(reason), _) => {
                    let reason = format!("denied by gate `{}`: {reason}", gated.gate);
                    refusals = refuse(refusals, id, reason);
                }
            }
        }
        if !refusals.content.is_empty() {
            effects.push(Effect::from(refusals.with_generated_id_if_missing()));
        }
        if waiting {
            yielded_with(effects)
        } else if effects.is_empty() {
            not_applicable()
        } else {
            applied(effects)
        }
    }
}

/// The step that stops a run while one of its tool calls waits for a signal.
///
/// A call that waits has a placeholder for a result until its signal is stored (see
/// [`corduroy_core::call`]). Placed after the tools and before the model, this step yields
/// while any call of the session still waits, so the model is never asked with a
/// placeholder in the conversation. The harness then runs what the call waits for, if it is
/// a child session, or leaves the run waiting in the log for [`Harness::signal`].
///
/// [`Harness::signal`]: crate::Harness::signal
#[derive(Debug)]
pub struct WaitOperation {
    sessions: Arc<Sessions>,
}

impl WaitOperation {
    /// Hold the sessions in `sessions` while their calls wait.
    #[must_use]
    pub const fn new(sessions: Arc<Sessions>) -> Self {
        Self { sessions }
    }
}

#[async_trait]
impl Operation<Session, Effect> for WaitOperation {
    fn name(&self) -> &'static str {
        "waits"
    }

    async fn run(
        &self,
        session: &Session,
        _conversation: &Conversation,
        _emit: &Emitter,
    ) -> Result<OperationResult<Effect>> {
        let journal = self.sessions.journal(&SessionId::new(session.id()))?;
        if journal.awaiting().is_empty() {
            not_applicable()
        } else {
            yielded_with(Vec::new())
        }
    }
}

/// The step that applies changes to the plugin tree that plugins asked for.
///
/// A plugin loading or unloading another through the `plugins` capability cannot change the
/// tree while the loop runs; this step applies such changes before every step, so that the
/// next one sees their tools, gates and prompt parts. It never applies itself.
pub struct SettleOperation(pub Arc<Mutex<App>>);

impl std::fmt::Debug for SettleOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SettleOperation")
    }
}

#[async_trait]
impl Operation<Session, Effect> for SettleOperation {
    fn name(&self) -> &'static str {
        "settle"
    }

    async fn run(
        &self,
        _session: &Session,
        _conversation: &Conversation,
        _emit: &Emitter,
    ) -> Result<OperationResult<Effect>> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .apply_requests();
        not_applicable()
    }
}
