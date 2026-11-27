//! Compaction: a summary in place of the start of a long conversation.
//!
//! The harness decides *where* a conversation may be cut and records the cut; a plugin
//! decides *when* and writes the summary. A plugin that provides the service named
//! [`COMPACTION`] is asked, before each step of the loop, whether the conversation is due:
//!
//! | Method | Payload | Answer |
//! | --- | --- | --- |
//! | `due` | `{"session", "input_tokens", "messages", "context_tokens", "overflowed"}` | `{"due": true}` or `{"due": false}` |
//! | `summarize` | `{"session", "transcript", "context_tokens"}` | `{"summary": "..."}` |
//!
//! `input_tokens` is what the next model request is expected to take: what the last one
//! took and was answered with, and an estimate, at three bytes of text to a token, of what
//! was added since (of everything, where no request was answered since the last summary).
//! `messages` is how many messages would give way to the summary. `context_tokens` is how
//! much the session's model holds at once, and is there only if the model's description
//! says: a service can then start from a share of it, and keep what it sends a summariser
//! within it. `overflowed` is there, and true, when the session's last run ended because
//! its request was more than the model holds: the conversation is then due whatever the
//! counts say, since nothing but a summary lets the session be used again.
//!
//! The cut falls where a step begins, so no tool call is parted from its result. While a
//! turn is under way and the calls of its last step are answered, the model keeps that
//! step and everything before it gives way, the start of the turn included; what the
//! person asked for is then added to the summary in their own words, since the model no
//! longer sees their message. Otherwise the cut is where the turn began. The summary is
//! stored as a `compaction` event in the step's commit, and
//! the projection hides the summarised messages from the model and puts the summary in their
//! place (see [`session`](crate::session)). With no such service, nothing happens.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;

use anyhow::Result;
use async_trait::async_trait;
use corduroy_core::{ModelInfo, Services};
use serde_json::{Value, json};

use crate::operation::{Next, Operation};
use crate::session::{COMPACTION, Effect, Session, Sessions};
use crate::tools::blocking;

/// The step that asks the [`COMPACTION`] service whether to summarise the start of the
/// conversation, and records the summary if so.
#[derive(Debug)]
pub struct CompactOperation {
    services: Arc<Services>,
    sessions: Arc<Sessions>,
    /// What is known about the model the sessions run on.
    model: ModelInfo,
}

impl CompactOperation {
    /// Ask the service in `services` about the sessions in `sessions`.
    #[must_use]
    pub const fn new(services: Arc<Services>, sessions: Arc<Sessions>) -> Self {
        Self {
            services,
            sessions,
            model: ModelInfo::unknown(),
        }
    }

    /// Tell the service what is known about the model the sessions run on: how much it
    /// holds, if `model` says.
    #[must_use]
    pub fn for_model(mut self, model: ModelInfo) -> Self {
        self.model = model;
        self
    }
}

/// What comes between a summary and the request of the turn it was made in.
const REQUEST_NOTE: &str = "\n\nWhat the person asked for, which is still being worked on, in \
their words:\n";
/// Most characters of the person's request kept with a summary.
const REQUEST: usize = 4000;

/// Call `method` of the compaction service as blocking work, since it may ask a model.
async fn ask(services: &Arc<Services>, method: &'static str, payload: Value) -> Option<Value> {
    let services = Arc::clone(services);
    let answer = blocking(move || services.call("host", COMPACTION, method, &payload.to_string()))
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
        let mut due = json!({
            "session": session.id().as_str(),
            "input_tokens": compactable.input_tokens,
            "messages": compactable.messages,
        });
        if let Some(asked) = due.as_object_mut() {
            if let Some(context) = self.model.context_tokens {
                asked.insert("context_tokens".to_owned(), json!(context));
            }
            if compactable.overflowed {
                asked.insert("overflowed".to_owned(), json!(true));
            }
        }
        // A service that fails, or answers what it should not, leaves the conversation be.
        let is_due = ask(&self.services, "due", due).await;
        if is_due.and_then(|answer| answer.get("due").and_then(Value::as_bool)) != Some(true) {
            return Ok(Next::Pass);
        }
        let transcript = journal.transcript_before(compactable.keep_from);
        let mut request = json!({"session": session.id().as_str(), "transcript": transcript});
        if let (Some(context), Some(asked)) = (self.model.context_tokens, request.as_object_mut()) {
            asked.insert("context_tokens".to_owned(), json!(context));
        }
        let summary = ask(&self.services, "summarize", request)
            .await
            .and_then(|answer| {
                answer
                    .get("summary")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .filter(|summary| !summary.trim().is_empty());
        let Some(mut summary) = summary else {
            return Ok(Next::Pass);
        };
        if let Some(asked) = compactable.request.filter(|asked| !asked.trim().is_empty()) {
            let asked: String = asked.chars().take(REQUEST).collect();
            summary.push_str(REQUEST_NOTE);
            summary.push_str(&asked);
        }
        Ok(Next::Continue(vec![Effect::Compacted {
            summary,
            keep_from: compactable.keep_from,
            by: COMPACTION.to_owned(),
        }]))
    }
}
