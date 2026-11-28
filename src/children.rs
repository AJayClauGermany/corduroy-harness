//! Child sessions: a tool call that hands a task to an agent of its own, and waits for it.
//!
//! The harness provides a service named [`SESSIONS`] to plugins. Its method `start`, called
//! while serving a tool call, starts a child session of the call's session for a task and
//! makes the call wait for the child's end (see [`corduroy_core::call`]). The run of the
//! parent stops before the model is asked again; the harness then runs the child, a run of
//! its own with the same model, tools and gates, and when it ends stores the signal
//! [`end_of`] the child with its answer, which becomes the call's result. The parent goes
//! on. Everything is in the log, so a crash anywhere in this is picked up by
//! [`Harness::continue_run`](crate::Harness::continue_run).
//!
//! | Method | Payload | Answer |
//! | --- | --- | --- |
//! | `start` | `{"task", "instructions"}` (instructions optional) | `{"session"}`, the child's id |
//!
//! A WebAssembly plugin reaches it through the `service` capability scoped to `sessions`;
//! the `subagents` plugin offers it to the model as `delegate`.
//!
//! **Starting a session is a decision point** ([`SESSION_START`]), which the harness
//! declares and asks the gates at before a child is started. That it is one is decided;
//! the request it sends is not (D3 and D2 in `docs/open-questions.md`), and the one here
//! is a prototype: `docs/prototypes.md` says what it is for and what it found. A start a
//! gate denies fails the call with the gate's reason, and no session is started. One a
//! gate asks about is refused too, since nothing can wait for a person's answer there yet.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec::Vec;

use corduroy_core::call::Serve;
use corduroy_core::{CallError, Gates, Service, SessionId, Verdict};
use serde_json::{Value, json};

use crate::session::{Sessions, end_of};

/// The decision point asked before a session is started.
///
/// A prototype of its request, which is JSON text: `{"session", "parent", "starter",
/// "task"}`, where `session` is the id the new session would have, `parent` the session
/// whose tool call starts it, and `starter` who asks, as an event's source is written
/// (`plugin:subagents`). It gains what a session is started with (a model, a loop,
/// instructions, grants, limits) as each is built.
pub const SESSION_START: &str = "session/start";

/// Name of the service through which plugins start child sessions.
pub const SESSIONS: &str = "sessions";

/// How deep child sessions may nest below the session a person started.
pub const MAX_DEPTH: usize = 3;

/// The [`SESSIONS`] service.
#[derive(Debug)]
pub(crate) struct SessionsService {
    pub(crate) sessions: Arc<Sessions>,
    pub(crate) gates: Arc<Gates>,
}

impl Service for SessionsService {
    fn call(&self, caller: &str, method: &str, payload: &str) -> Result<String, CallError> {
        if method != "start" {
            return Err(CallError::Unknown(format!("method `{method}`")));
        }
        let payload: Value = serde_json::from_str(payload)
            .map_err(|error| CallError::InvalidInput(error.to_string()))?;
        let task = payload
            .get("task")
            .and_then(Value::as_str)
            .filter(|task| !task.trim().is_empty())
            .ok_or_else(|| CallError::InvalidInput("`task` is required".to_owned()))?;
        let task = match payload.get("instructions").and_then(Value::as_str) {
            Some(instructions) if !instructions.trim().is_empty() => {
                format!("{instructions}\n\n{task}")
            }
            _ => task.to_owned(),
        };
        let scope = corduroy_core::call::current().ok_or_else(|| {
            CallError::Denied(format!(
                "`{caller}` may start a session only while serving a tool call"
            ))
        })?;
        let parent = scope.session;
        let depth = self.depth(&parent);
        if depth >= MAX_DEPTH {
            return Err(CallError::Denied(format!(
                "sessions nest at most {MAX_DEPTH} deep; `{parent}` is {depth} deep"
            )));
        }
        // The first free `<parent>/<n>`: a person may have started one by that name.
        let taken: Vec<SessionId> = self
            .sessions
            .list()
            .into_iter()
            .map(|info| info.id)
            .collect();
        // One more name than there are sessions is always enough to find a free one.
        let child = (1..=taken.len() + 1)
            .map(|n| SessionId::new(format!("{parent}/{n}")))
            .find(|id| !taken.contains(id))
            .ok_or_else(|| CallError::Failed("no session name is free".to_owned()))?;
        self.gate(caller, &parent, &child, &task)?;
        let child = self
            .sessions
            .start_with(child, Some(&parent), Some(task))
            .map_err(|error| CallError::Failed(error.to_string()))?;
        if !corduroy_core::call::wait_for(&end_of(&child)) {
            return Err(CallError::Failed("the call cannot wait".to_owned()));
        }
        Ok(json!({ "session": child.as_str() }).to_string())
    }
}

impl SessionsService {
    /// Ask the gates whether `caller` may start `child` under `parent` for `task`, and
    /// record what they said in the parent's session. Nothing is recorded if no gate has an
    /// opinion.
    fn gate(
        &self,
        caller: &str,
        parent: &SessionId,
        child: &SessionId,
        task: &str,
    ) -> Result<(), CallError> {
        let request = json!({
            "session": child.as_str(),
            "parent": parent.as_str(),
            "starter": format!("plugin:{caller}"),
            "task": task,
        });
        // The gates are asked for the session that starts the other, without its tool
        // call: a gate that waited there would hold the call it is asked about.
        let asked = Serve::session(parent.clone());
        let (decision, _) = asked.run(|| self.gates.decide(SESSION_START, &request.to_string()));
        let Some(decision) = decision else {
            return Ok(());
        };
        let mut recorded = request;
        if let Some(fields) = recorded.as_object_mut() {
            fields.insert("point".to_owned(), json!(SESSION_START));
            fields.insert("verdict".to_owned(), json!(decision.verdict.kind()));
            fields.insert("reason".to_owned(), json!(decision.verdict.reason()));
            fields.insert("gate".to_owned(), json!(decision.gate));
        }
        self.sessions
            .journal(parent)
            .and_then(|journal| Ok(journal.noted("gate/decision", &recorded)?))
            .map_err(|error| CallError::Failed(error.to_string()))?;
        match decision.verdict {
            Verdict::Allow => Ok(()),
            Verdict::Deny(reason) => Err(CallError::Denied(format!(
                "denied by gate `{}`: {reason}",
                decision.gate
            ))),
            Verdict::Ask(reason) => Err(CallError::Denied(format!(
                "gate `{}` asked for a person's approval, which starting a session cannot \
                 wait for yet: {reason}",
                decision.gate
            ))),
        }
    }

    /// How many parents `session` has.
    fn depth(&self, session: &SessionId) -> usize {
        let all = self.sessions.list();
        let mut depth = 0;
        let mut current = session.clone();
        while let Some(parent) = all
            .iter()
            .find(|info| info.id == current)
            .and_then(|info| info.parent.clone())
        {
            depth += 1;
            current = parent;
            if depth > MAX_DEPTH {
                break;
            }
        }
        depth
    }
}
