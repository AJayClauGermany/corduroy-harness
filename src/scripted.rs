//! A model that replays a script, for tests and demos. No network, no LLM.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use corduroy_core::{Message, ToolCall};
use serde_json::Value;

use crate::provider::{ModelReply, ModelRequest, Provider, ProviderError, Usage};

/// What a [`ScriptedProvider`] was asked on one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The system prompt.
    pub system: String,
    /// Names of the tools on offer.
    pub tools: Vec<String>,
    /// How many messages the conversation held.
    pub messages: usize,
    /// The conversation as it was sent.
    pub conversation: Vec<Message>,
}

/// A [`Provider`] that returns prepared replies in order and records what it was asked.
#[derive(Debug, Default)]
pub struct ScriptedProvider {
    replies: Mutex<VecDeque<Message>>,
    requests: Mutex<Vec<Request>>,
}

impl ScriptedProvider {
    /// A provider that will give `replies` in order, one per request.
    #[must_use]
    pub fn new(replies: impl IntoIterator<Item = Message>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Every request received so far.
    #[must_use]
    pub fn requests(&self) -> Vec<Request> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// A reply in which the model says `text` and ends its turn.
#[must_use]
pub fn say(text: &str) -> Message {
    Message::assistant().text(text)
}

/// A reply in which the model calls `tool` with `input`, which must be a JSON object.
#[must_use]
pub fn call(id: &str, tool: &str, input: &Value) -> Message {
    Message::assistant().call(ToolCall::new(id, tool, input.to_string()))
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn complete(&self, request: &ModelRequest) -> Result<ModelReply, ProviderError> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Request {
                system: request.system.clone(),
                tools: request.tools.iter().map(|tool| tool.name.clone()).collect(),
                messages: request.messages.len(),
                conversation: request.messages.clone(),
            });
        let reply = self
            .replies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .ok_or_else(|| ProviderError::new("the script ran out"))?;
        Ok(ModelReply::new(reply, Usage::default()))
    }
}
