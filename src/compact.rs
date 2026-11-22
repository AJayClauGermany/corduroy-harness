//! Compaction: a summary in place of the start of a long conversation.
//!
//! The harness decides *where* a conversation may be cut and records the cut; a plugin
//! decides *when* and writes the summary. A plugin that provides the service named
//! [`COMPACTION`] is asked, before each step of the loop, whether the conversation is due:
//!
//! | Method | Payload | Answer |
//! | --- | --- | --- |
//! | `due` | `{"session", "input_tokens", "messages"}` | `{"due": true}` or `{"due": false}` |
//! | `summarize` | `{"session", "transcript"}` | `{"summary": "..."}` |
//!
//! `input_tokens` is what the last model request took (zero if a compaction came after it)
//! and `messages` how many messages the model still sees from before the current turn. Only
//! those are ever summarised: the turn in progress is kept whole, so no tool call is parted
//! from its result. The summary is stored as a `compaction` event in the step's commit, and
//! the projection hides the summarised messages from the model and puts the summary in their
//! place (see [`session`](crate::session)). With no such service, nothing happens.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use corduroy_core::Services;
use serde_json::{Value, json};

use crate::operation::{Next, Operation};
use crate::session::{COMPACTION, Effect, Session, Sessions};

/// The step that asks the [`COMPACTION`] service whether to summarise the start of the
/// conversation, and records the summary if so.
#[derive(Debug)]
pub struct CompactOperation {
    services: Arc<Services>,
    sessions: Arc<Sessions>,
}

impl CompactOperation {
    /// Ask the service in `services` about the sessions in `sessions`.
    #[must_use]
    pub const fn new(services: Arc<Services>, sessions: Arc<Sessions>) -> Self {
        Self { services, sessions }
    }
}

/// Call `method` of the compaction service on a blocking thread, since it may ask a model.
async fn ask(services: &Arc<Services>, method: &'static str, payload: Value) -> Option<Value> {
    let services = Arc::clone(services);
    let answer = tokio::task::spawn_blocking(move || {
        services.call("host", COMPACTION, method, &payload.to_string())
    })
    .await
    .ok()?
    .ok()?;
    serde_json::from_str(&answer).ok()
}

#[async_trait]
impl Operation for CompactOperation {
    fn name(&self) -> &'static str {
        "compact"
    }

    async fn run(&self, session: &Session) -> Result<Next> {
        if self.services.get(COMPACTION).is_none() {
            return Ok(Next::Pass);
        }
        let journal = self.sessions.journal(session.id())?;
        let Some(compactable) = journal.compactable() else {
            return Ok(Next::Pass);
        };
        let due = json!({
            "session": session.id().as_str(),
            "input_tokens": compactable.input_tokens,
            "messages": compactable.messages,
        });
        // A service that fails, or answers what it should not, leaves the conversation be.
        let is_due = ask(&self.services, "due", due).await;
        if is_due.and_then(|answer| answer.get("due").and_then(Value::as_bool)) != Some(true) {
            return Ok(Next::Pass);
        }
        let transcript = journal.transcript_before(compactable.keep_from);
        let request = json!({"session": session.id().as_str(), "transcript": transcript});
        let summary = ask(&self.services, "summarize", request)
            .await
            .and_then(|answer| {
                answer
                    .get("summary")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .filter(|summary| !summary.trim().is_empty());
        let Some(summary) = summary else {
            return Ok(Next::Pass);
        };
        Ok(Next::Continue(vec![Effect::Compacted {
            summary,
            keep_from: compactable.keep_from,
            by: COMPACTION.to_owned(),
        }]))
    }
}
