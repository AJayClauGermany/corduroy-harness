//! A model that replays a script, for tests and demos. No network, no LLM.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;

use async_trait::async_trait;
use corduroy_core::sync::Lock;
use corduroy_core::{Message, ToolCall};
use serde_json::Value;

use corduroy_core::{ModelReply, ModelRequest, Usage};

use crate::provider::{Provider, ProviderError};

/// What a [`ScriptedProvider`] was asked on one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The model asked, by the provider's own name for it.
    pub model: String,
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
    replies: Lock<VecDeque<Message>>,
    requests: Lock<Vec<Request>>,
}

impl ScriptedProvider {
    /// A provider that will give `replies` in order, one per request.
    #[must_use]
    pub fn new(replies: impl IntoIterator<Item = Message>) -> Self {
        Self {
            replies: Lock::new(replies.into_iter().collect()),
            requests: Lock::new(Vec::new()),
        }
    }

    /// Every request received so far.
    #[must_use]
    pub fn requests(&self) -> Vec<Request> {
        self.requests.with(|requests| requests.clone())
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
        let asked = Request {
            model: request.model.clone(),
            system: request.system.clone(),
            tools: request.tools.iter().map(|tool| tool.name.clone()).collect(),
            messages: request.messages.len(),
            conversation: request.messages.clone(),
        };
        self.requests.with(|requests| requests.push(asked));
        let reply = self
            .replies
            .with(VecDeque::pop_front)
            .ok_or_else(|| ProviderError::new("the script ran out"))?;
        Ok(ModelReply::new(reply, Usage::default()))
    }
}
