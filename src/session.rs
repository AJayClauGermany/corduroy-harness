//! The conversation as a projection of the event stream.
//!
//! oi-kit's state machine never mutates a conversation. Its operations return [`Effect`]s,
//! and the host decides what persisting them means. Here it means appending to the event
//! stream: the [`Journal`] turns each effect into events, and the conversation a model sees
//! is whatever the `message` events add up to. A run can therefore be resumed, replayed or
//! inspected from the log alone, and every plugin sees the same history the model does.
//!
//! | Topic | Source | Payload |
//! | --- | --- | --- |
//! | `message` | `user`, `agent` or `environment` | the oi-kit message, as JSON |
//! | `tool/call` | `agent` | `{"id", "tool", "input"}`, caused by its `message` |
//! | `tool/result` | `environment` | `{"id", "tool", "ok", "output"}`, caused by its `tool/call` |
//! | `usage` | `agent` | the provider's usage report, as JSON |
//! | `turn/end` | `host` | empty |

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use corduroy_core::{Draft, EventId, EventStream, Pattern, Source};
use oi_kit_agent::inference::InferenceEffect;
use oi_kit_agent::machine::{EffectHandler, MachineSession, SessionLoader};
use oi_kit_agent::operation::{Emitter, MachineEffect};
use oi_kit_provider_types::conversation::Conversation;
use oi_kit_provider_types::conversation::message::{Message, MessageContent};
use oi_kit_provider_types::conversation::token_usage::ProviderUsage;
use rmcp::model::Role;
use serde_json::{Value, json};

/// Topic of the events that make up the conversation.
pub const MESSAGE: &str = "message";

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

/// Records a session's effects on the event stream and rebuilds the session from it.
pub struct Journal {
    id: String,
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
    /// holds. Pass a stream rebuilt with [`EventStream::replay`] to resume an earlier run.
    ///
    /// # Errors
    ///
    /// Fails if a `message` or `tool/call` event in the stream cannot be decoded.
    pub fn new(id: impl Into<String>, events: Arc<EventStream>) -> Result<Self> {
        let mut conversation = Conversation::empty();
        for event in events.matching(&Pattern::new(MESSAGE)) {
            let message = serde_json::from_str(&event.payload)
                .with_context(|| format!("event {} is not a message", event.id))?;
            conversation.push(message);
        }
        let mut calls = BTreeMap::new();
        for event in events.matching(&Pattern::new("tool/call")) {
            let call: Value = serde_json::from_str(&event.payload)
                .with_context(|| format!("event {} is not a tool call", event.id))?;
            let field = |name: &str| call.get(name).and_then(Value::as_str).map(str::to_owned);
            if let (Some(id), Some(tool)) = (field("id"), field("tool")) {
                calls.insert(id, (event.id, tool));
            }
        }
        Ok(Self {
            id: id.into(),
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
            id: self.id.clone(),
            conversation: self.state().conversation.clone(),
        }
    }

    /// Append `message` to the conversation and publish it, along with an event for each
    /// tool call and tool result it carries.
    ///
    /// # Errors
    ///
    /// Fails if the message cannot be encoded as JSON.
    pub fn record(&self, message: &Message) -> Result<EventId> {
        let payload = serde_json::to_string(message).context("message is not valid JSON")?;
        let source = match message.role {
            Role::Assistant => Source::Agent,
            Role::User if message.is_tool_response() => Source::Environment,
            Role::User => Source::User,
        };
        self.state().conversation.push(message.clone());
        // Emitting happens with the state unlocked: a listener may record in turn.
        let recorded = self
            .events
            .emit(Draft::new(source, MESSAGE).payload(payload));
        for content in &message.content {
            match content {
                MessageContent::ToolRequest(request) => {
                    let (tool, input) = request.tool_call.as_ref().map_or_else(
                        |error| ("<malformed>".to_owned(), json!({"error": error.message})),
                        |call| (call.name.to_string(), json!(call.arguments)),
                    );
                    let payload = json!({"id": request.id, "tool": tool, "input": input});
                    let draft = Draft::new(Source::Agent, "tool/call")
                        .payload(payload.to_string())
                        .cause(recorded);
                    let call = self.events.emit(draft);
                    self.state().calls.insert(request.id.clone(), (call, tool));
                }
                MessageContent::ToolResponse(response) => {
                    let (cause, tool) = self
                        .state()
                        .calls
                        .remove(&response.id)
                        .unwrap_or((recorded, String::new()));
                    let (ok, output) = match &response.tool_result {
                        Ok(result) => (result.is_error != Some(true), text_of(result)),
                        Err(error) => (false, error.message.to_string()),
                    };
                    let payload =
                        json!({"id": response.id, "tool": tool, "ok": ok, "output": output});
                    let draft = Draft::new(Source::Environment, "tool/result")
                        .payload(payload.to_string())
                        .cause(cause);
                    self.events.emit(draft);
                }
                _ => {}
            }
        }
        Ok(recorded)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
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
        for effect in effects {
            match effect {
                Effect::Message(message) => {
                    self.record(message)?;
                }
                Effect::Usage(usage) => {
                    let payload = serde_json::to_string(usage).context("usage is not JSON")?;
                    self.events
                        .emit(Draft::new(Source::Agent, "usage").payload(payload));
                }
            }
        }
        Ok(())
    }
}
