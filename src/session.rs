//! The conversation as a projection of the event stream.
//!
//! oi-kit's state machine never mutates a conversation. Its operations return [`Effect`]s,
//! and the host decides what persisting them means. Here it means appending to the event
//! stream: the [`Journal`] turns each effect into events, and the conversation a model sees
//! is whatever the `message` events add up to. A run can therefore be resumed, replayed or
//! inspected from the log alone, and every plugin sees the same history the model does.
//!
//! One stream holds many sessions. Each begins with a `session/start` event, and every event
//! below names the session it belongs to, so a session is rebuilt from its own events alone.
//!
//! | Topic | Source | Payload |
//! | --- | --- | --- |
//! | `session/start` | `host` | `{"parent"}`, or `{}` for a session with no parent |
//! | `message` | `user`, `agent` or `environment` | the oi-kit message, as JSON |
//! | `tool/call` | `agent` | `{"id", "tool", "input"}`, caused by its `message` |
//! | `tool/result` | `environment` | `{"id", "tool", "ok", "output"}`, caused by its `tool/call` |
//! | `usage` | `agent` | the provider's usage report, as JSON |
//! | `turn/end` | `host` | empty |

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use corduroy_core::{Draft, Event, EventId, EventStream, Pattern, SessionId, Source};
use oi_kit_agent::inference::InferenceEffect;
use oi_kit_agent::machine::{EffectHandler, MachineSession, SessionLoader};
use oi_kit_agent::operation::{Emitter, MachineEffect};
use oi_kit_provider_types::conversation::Conversation;
use oi_kit_provider_types::conversation::message::{
    Message, MessageContent, ToolRequest, ToolResponse,
};
use oi_kit_provider_types::conversation::token_usage::ProviderUsage;
use rmcp::model::Role;
use serde_json::{Value, json};

/// Topic of the events that make up the conversation.
pub const MESSAGE: &str = "message";

/// Topic of the event that begins a session.
pub const SESSION_START: &str = "session/start";

/// Name of the session a harness starts with, and the one [`Harness::run`] uses.
///
/// [`Harness::run`]: crate::Harness::run
pub const MAIN: &str = "main";

/// A session and where it came from, as its `session/start` event recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    /// The session.
    pub id: SessionId,
    /// The session that started it, if one did.
    pub parent: Option<SessionId>,
}

impl SessionInfo {
    /// The event that begins this session.
    pub(crate) fn start(&self) -> Draft {
        let payload = self
            .parent
            .as_ref()
            .map_or_else(|| json!({}), |parent| json!({"parent": parent.as_str()}));
        Draft::new(Source::Host, SESSION_START)
            .session(self.id.clone())
            .payload(payload.to_string())
    }

    /// Every session `events` holds, in the order they were started.
    pub(crate) fn all(events: &EventStream) -> Vec<Self> {
        events
            .matching(&Pattern::new(SESSION_START))
            .into_iter()
            .filter_map(|event| {
                let payload: Value = serde_json::from_str(&event.payload).ok()?;
                let parent = payload.get("parent").and_then(Value::as_str);
                Some(Self {
                    id: event.session?,
                    parent: parent.map(SessionId::new),
                })
            })
            .collect()
    }
}

/// One conversation, as the state machine sees it at the start of a step.
#[derive(Debug, Clone)]
pub struct Session {
    id: String,
    conversation: Conversation,
}

impl Session {
    /// The messages so far.
    #[must_use]
    pub const fn conversation(&self) -> &Conversation {
        &self.conversation
    }
}

impl MachineSession for Session {
    fn id(&self) -> &str {
        &self.id
    }

    fn conversation(&self) -> Option<&Conversation> {
        Some(&self.conversation)
    }
}

/// Something an operation wants recorded.
#[derive(Debug)]
pub enum Effect {
    /// Append a message to the conversation.
    Message(Box<Message>),
    /// Note what a model request cost.
    Usage(Box<ProviderUsage>),
}

impl From<Message> for Effect {
    fn from(message: Message) -> Self {
        Self::Message(Box::new(message))
    }
}

impl InferenceEffect for Effect {
    fn record_usage(usage: ProviderUsage) -> Self {
        Self::Usage(Box::new(usage))
    }
}

impl MachineEffect for Effect {
    fn ensure_message_ids(&mut self) {
        if let Self::Message(message) = self
            && message.id.is_none()
        {
            **message = message.as_ref().clone().with_generated_id_if_missing();
        }
    }
}

struct State {
    conversation: Conversation,
    /// The `tool/call` event and tool name behind each open tool-call id.
    calls: BTreeMap<String, (EventId, String)>,
}

/// One step's worth of events, and what they change in the projection once stored.
#[derive(Default)]
struct Batch {
    drafts: Vec<Draft>,
    messages: Vec<Message>,
    /// Tool-call id to the position of its `tool/call` draft and the tool's name.
    opened: BTreeMap<String, (usize, String)>,
    /// Ids of the tool calls answered in this batch.
    closed: Vec<String>,
}

impl Batch {
    /// Add the `tool/call` event for `request`, caused by the message at `message`.
    fn call(&mut self, request: &ToolRequest, message: usize) {
        let (tool, input) = request.tool_call.as_ref().map_or_else(
            |error| ("<malformed>".to_owned(), json!({"error": error.message})),
            |call| (call.name.to_string(), json!(call.arguments)),
        );
        let payload = json!({"id": request.id, "tool": tool, "input": input});
        self.opened
            .insert(request.id.clone(), (self.drafts.len(), tool));
        self.drafts.push(
            Draft::new(Source::Agent, "tool/call")
                .payload(payload.to_string())
                .cause_in_batch(message),
        );
    }

    /// Add the `tool/result` event for `response`. It is caused by its call, which is in this
    /// batch, or is the `open` one from an earlier commit, or else is unknown, in which case
    /// the message at `message` stands in.
    fn result(&mut self, response: &ToolResponse, open: Option<(EventId, String)>, message: usize) {
        let draft = Draft::new(Source::Environment, "tool/result");
        let (draft, tool) = match (self.opened.get(&response.id), open) {
            (Some((call, tool)), _) => (draft.cause_in_batch(*call), tool.clone()),
            (None, Some((call, tool))) => (draft.cause(call), tool),
            (None, None) => (draft.cause_in_batch(message), String::new()),
        };
        let (ok, output) = match &response.tool_result {
            Ok(result) => (result.is_error != Some(true), text_of(result)),
            Err(error) => (false, error.message.to_string()),
        };
        let payload = json!({"id": response.id, "tool": tool, "ok": ok, "output": output});
        self.drafts.push(draft.payload(payload.to_string()));
        self.closed.push(response.id.clone());
    }
}

/// Records a session's effects on the event stream and rebuilds the session from it.
///
/// The effects of one step of the loop become one commit: they are stored together or not
/// at all, and the conversation the next step sees changes only once they are.
pub struct Journal {
    id: SessionId,
    events: Arc<EventStream>,
    state: Mutex<State>,
}

impl std::fmt::Debug for Journal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Journal")
            .field("id", &self.id)
            .field("messages", &self.state().conversation.len())
            .finish_non_exhaustive()
    }
}

impl Journal {
    /// A journal for the session `id`, picking up whatever conversation `events` already
    /// holds for it. Pass a stream rebuilt with [`EventStream::replay`] to resume an earlier
    /// run.
    ///
    /// # Errors
    ///
    /// Fails if a `message` or `tool/call` event of the session cannot be decoded.
    pub fn new(id: SessionId, events: Arc<EventStream>) -> Result<Self> {
        let mut conversation = Conversation::empty();
        let mut calls = BTreeMap::new();
        for event in events.session(&id) {
            if event.topic == MESSAGE {
                let message = serde_json::from_str(&event.payload)
                    .with_context(|| format!("event {} is not a message", event.id))?;
                conversation.push(message);
            } else if event.topic == "tool/call"
                && let Some((id, tool)) = call_of(&event)?
            {
                calls.insert(id, (event.id, tool));
            }
        }
        Ok(Self {
            id,
            events,
            state: Mutex::new(State {
                conversation,
                calls,
            }),
        })
    }

    /// The session as it stands.
    #[must_use]
    pub fn session(&self) -> Session {
        Session {
            id: self.id.as_str().to_owned(),
            conversation: self.state().conversation.clone(),
        }
    }

    /// Append `message` to the conversation and publish it, along with an event for each
    /// tool call and tool result it carries, as one commit.
    ///
    /// # Errors
    ///
    /// Fails if the message cannot be encoded as JSON or the commit cannot be stored.
    pub fn record(&self, message: &Message) -> Result<EventId> {
        let mut batch = Batch::default();
        let position = self.draft_message(&mut batch, message)?;
        let ids = self.commit(batch)?;
        ids.get(position)
            .copied()
            .context("the commit returned no id for the message")
    }

    /// Add `message` to `batch`, with an event for each tool call and tool result it carries.
    /// Returns the position of the `message` event in the batch.
    fn draft_message(&self, batch: &mut Batch, message: &Message) -> Result<usize> {
        let payload = serde_json::to_string(message).context("message is not valid JSON")?;
        let source = match message.role {
            Role::Assistant => Source::Agent,
            Role::User if message.is_tool_response() => Source::Environment,
            Role::User => Source::User,
        };
        let recorded = batch.drafts.len();
        batch
            .drafts
            .push(Draft::new(source, MESSAGE).payload(payload));
        batch.messages.push(message.clone());
        for content in &message.content {
            match content {
                MessageContent::ToolRequest(request) => batch.call(request, recorded),
                MessageContent::ToolResponse(response) => {
                    let open = self.state().calls.get(&response.id).cloned();
                    batch.result(response, open, recorded);
                }
                _ => {}
            }
        }
        Ok(recorded)
    }

    /// The session this journal records.
    #[must_use]
    pub const fn id(&self) -> &SessionId {
        &self.id
    }

    /// Record an event of this session that is not part of the conversation.
    pub(crate) fn note(&self, source: Source, topic: &str) -> Result<EventId> {
        Ok(self
            .events
            .emit(Draft::new(source, topic).session(self.id.clone()))?)
    }

    /// Store `batch` as one commit of this session, then bring the projection up to date.
    fn commit(&self, batch: Batch) -> Result<Vec<EventId>> {
        let drafts = batch
            .drafts
            .into_iter()
            .map(|draft| draft.session(self.id.clone()));
        let ids = self.events.commit(drafts)?;
        let mut state = self.state();
        for message in batch.messages {
            state.conversation.push(message);
        }
        for (call, (position, tool)) in batch.opened {
            if let Some(id) = ids.get(position) {
                state.calls.insert(call, (*id, tool));
            }
        }
        for call in batch.closed {
            state.calls.remove(&call);
        }
        drop(state);
        Ok(ids)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The tool-call id and tool name a `tool/call` event records.
fn call_of(event: &Event) -> Result<Option<(String, String)>> {
    let call: Value = serde_json::from_str(&event.payload)
        .with_context(|| format!("event {} is not a tool call", event.id))?;
    let field = |name: &str| call.get(name).and_then(Value::as_str).map(str::to_owned);
    Ok(field("id").zip(field("tool")))
}

/// The text blocks of a tool result, joined.
fn text_of(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[async_trait]
impl SessionLoader<Session> for Journal {
    async fn load(&self, _session_id: &str) -> Result<Session> {
        Ok(self.session())
    }
}

#[async_trait]
impl EffectHandler<Session, Effect> for Journal {
    async fn apply_effects(
        &self,
        _session: &Session,
        effects: &mut [Effect],
        _emit: &Emitter,
    ) -> Result<()> {
        let mut batch = Batch::default();
        for effect in effects {
            match effect {
                Effect::Message(message) => {
                    self.draft_message(&mut batch, message)?;
                }
                Effect::Usage(usage) => {
                    let payload = serde_json::to_string(usage).context("usage is not JSON")?;
                    batch
                        .drafts
                        .push(Draft::new(Source::Agent, "usage").payload(payload));
                }
            }
        }
        self.commit(batch)?;
        Ok(())
    }
}
