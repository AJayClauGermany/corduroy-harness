//! A model that replays a script, for tests and demos. No network, no LLM.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use oi_kit_provider_types::base::{MessageStream, Provider};
use oi_kit_provider_types::conversation::message::Message;
use oi_kit_provider_types::conversation::token_usage::{ProviderUsage, Usage};
use oi_kit_provider_types::errors::ProviderError;
use oi_kit_provider_types::model::ModelConfig;
use rmcp::model::{CallToolRequestParams, Tool};
use serde_json::Value;

/// What a [`ScriptedProvider`] was asked on one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The system prompt.
    pub system: String,
    /// Names of the tools on offer.
    pub tools: Vec<String>,
    /// How many messages the conversation held.
    pub messages: usize,
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
    Message::assistant().with_text(text)
}

/// A reply in which the model calls `tool` with `input`, which must be a JSON object.
#[must_use]
pub fn call(id: &str, tool: &str, input: &Value) -> Message {
    let mut request = CallToolRequestParams::new(tool.to_owned());
    if let Some(arguments) = input.as_object() {
        request = request.with_arguments(arguments.clone());
    }
    Message::assistant().with_tool_request(id, Ok(request))
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn get_name(&self) -> &'static str {
        "scripted"
    }

    async fn stream(
        &self,
        _model_config: &ModelConfig,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<MessageStream, ProviderError> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Request {
                system: system.to_owned(),
                tools: tools.iter().map(|tool| tool.name.to_string()).collect(),
                messages: messages.len(),
            });
        let reply = self
            .replies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .ok_or_else(|| ProviderError::ExecutionError("the script ran out".to_owned()))?;
        let usage = ProviderUsage::new("scripted".to_owned(), Usage::default());
        Ok(Box::pin(futures::stream::once(async move {
            Ok((Some(reply), Some(usage)))
        })))
    }
}
