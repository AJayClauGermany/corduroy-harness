//! A model that replays a script, for tests and demos. No network, no LLM.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;

use core::future::{pending, poll_fn};
use core::task::Poll;

use async_trait::async_trait;
use corduroy_core::sync::Lock;
use corduroy_core::{Message, ToolCall};
use serde_json::Value;

use corduroy_core::{ModelReply, ModelRequest, Usage};

use crate::provider::{Piece, Provider, ProviderError, Reports, pieces_of};

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
    /// How many characters of an answer's text are told at a time, if it is told in
    /// pieces.
    piecewise: Option<usize>,
    /// What a request tells and then never answers, once the replies have run out.
    hanging: Option<Vec<Piece>>,
}

impl ScriptedProvider {
    /// A provider that will give `replies` in order, one per request.
    #[must_use]
    pub fn new(replies: impl IntoIterator<Item = Message>) -> Self {
        Self {
            replies: Lock::new(replies.into_iter().collect()),
            requests: Lock::new(Vec::new()),
            piecewise: None,
            hanging: None,
        }
    }

    /// Tell each answer as a model that streams would: its text `chars` characters at a
    /// time, with a pause between pieces in which whoever drives the run may do something
    /// else. The reply is the same whole one.
    #[must_use]
    pub const fn piecewise(mut self, chars: usize) -> Self {
        self.piecewise = Some(chars);
        self
    }

    /// Once the replies have run out, have a request tell `pieces` and then never answer,
    /// as a model that does not stop writing: for showing what a stop or a time limit
    /// leaves of it.
    #[must_use]
    pub fn hanging(mut self, pieces: impl IntoIterator<Item = Piece>) -> Self {
        self.hanging = Some(pieces.into_iter().collect());
        self
    }

    /// Note what `request` asked.
    fn asked(&self, request: &ModelRequest) {
        let asked = Request {
            model: request.model.clone(),
            system: request.system.clone(),
            tools: request.tools.iter().map(|tool| tool.name.clone()).collect(),
            messages: request.messages.len(),
            conversation: request.messages.clone(),
        };
        self.requests.with(|requests| requests.push(asked));
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
        self.complete_reporting(request, &()).await
    }

    async fn complete_reporting(
        &self,
        request: &ModelRequest,
        reports: &dyn Reports,
    ) -> Result<ModelReply, ProviderError> {
        self.asked(request);
        let Some(reply) = self.replies.with(VecDeque::pop_front) else {
            let Some(pieces) = &self.hanging else {
                return Err(ProviderError::new("the script ran out"));
            };
            for piece in pieces {
                reports.piece(piece);
                pause().await;
            }
            return pending().await;
        };
        let reply = ModelReply::new(reply, Usage::default());
        for piece in pieces_of(&reply) {
            match (&piece, self.piecewise) {
                (Piece::Text(text), Some(chars)) => {
                    let all: Vec<char> = text.chars().collect();
                    for some in all.chunks(chars.max(1)) {
                        reports.piece(&Piece::Text(some.iter().collect()));
                        pause().await;
                    }
                }
                _ => reports.piece(&piece),
            }
        }
        Ok(reply)
    }
}

/// Give way once to whatever else the executor has to do.
async fn pause() {
    let mut paused = false;
    poll_fn(|cx| {
        if paused {
            return Poll::Ready(());
        }
        paused = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    })
    .await;
}
