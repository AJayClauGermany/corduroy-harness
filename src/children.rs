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

use std::sync::Arc;

use corduroy_core::{CallError, Service, SessionId};
use serde_json::{Value, json};

use crate::session::{Sessions, end_of};

/// Name of the service through which plugins start child sessions.
pub const SESSIONS: &str = "sessions";

/// How deep child sessions may nest below the session a person started.
pub const MAX_DEPTH: usize = 3;

/// The [`SESSIONS`] service.
#[derive(Debug)]
pub(crate) struct SessionsService {
    pub(crate) sessions: Arc<Sessions>,
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
