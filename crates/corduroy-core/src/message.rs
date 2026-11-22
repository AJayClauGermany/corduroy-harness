//! Messages: what a conversation is made of.
//!
//! A conversation is a list of [`Message`]s, each said by the person or the model
//! ([`Role`]) and made of [`Part`]s: text, images, the model's thinking, the tool calls the
//! model asks for and the results that answer them. This is the shape the harness keeps in
//! its log (as `message` events) and the shape plugins read through `log-read`, so it is
//! the harness's own: model providers convert to and from it, and whatever shape a
//! provider's API has stays at that boundary.
//!
//! The JSON form, which the `message` event's payload holds:
//!
//! ```json
//! {"role": "assistant", "parts": [
//!   {"type": "text", "text": "I'll look."},
//!   {"type": "thinking", "text": "...", "signature": "..."},
//!   {"type": "tool_call", "id": "c1", "name": "read", "input": {"path": "a.txt"}},
//!   {"type": "tool_result", "id": "c1", "ok": true, "content": [{"type": "text", "text": "..."}]},
//!   {"type": "image", "mime_type": "image/png", "data": "<base64>"}
//! ]}
//! ```
//!
//! The core has no JSON codec; `corduroy-harness` reads and writes this form.
//!
//! ```
//! use corduroy_core::{Message, Role, ToolCall, ToolOutput, ToolResult};
//!
//! let asked = Message::assistant()
//!     .text("I'll read it.")
//!     .call(ToolCall::new("c1", "read", r#"{"path":"a.txt"}"#));
//! let answered = Message::user().result(ToolResult::new("c1", true, ToolOutput::text("hello")));
//!
//! assert_eq!(asked.tool_calls().count(), 1);
//! assert!(answered.is_tool_results());
//! assert_eq!(asked.text_content(), "I'll read it.");
//! assert_eq!(answered.role, Role::User);
//! ```

use crate::model::Role;
use crate::tool::ToolOutput;
use alloc::string::String;
use alloc::vec::Vec;

/// One part of a [`Message`].
///
/// More kinds may follow, so code that takes parts apart needs an arm for the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Part {
    /// Text.
    Text(String),
    /// An image.
    Image {
        /// What kind of image `data` is, such as `image/png`.
        mime_type: String,
        /// The image's bytes, as they would be in a file.
        data: Vec<u8>,
    },
    /// The model's reasoning before it answered. Some providers sign it and need it back,
    /// unchanged, on the next request.
    Thinking {
        /// What the model thought, as far as the provider shows it.
        text: String,
        /// The provider's signature over it, if it gave one.
        signature: Option<String>,
    },
    /// A tool call the model asks for.
    ToolCall(ToolCall),
    /// The result of a tool call, sent back to the model.
    ToolResult(ToolResult),
}

/// A tool call the model asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolCall {
    /// The call's id, unique within its session. Its result carries the same id.
    pub id: String,
    /// The tool asked for.
    pub name: String,
    /// The input, a JSON object as text.
    pub input: String,
}

impl ToolCall {
    /// A call `id` of the tool `name` with `input`, a JSON object as text.
    #[must_use]
    pub fn new(id: impl Into<String>, name: impl Into<String>, input: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            input: input.into(),
        }
    }
}

/// The result of a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolResult {
    /// The id of the call this answers.
    pub id: String,
    /// Whether the call succeeded. A failed call's output says why.
    pub ok: bool,
    /// What the tool returned, or why it failed.
    pub output: ToolOutput,
}

impl ToolResult {
    /// The result of the call `id`.
    #[must_use]
    pub fn new(id: impl Into<String>, ok: bool, output: impl Into<ToolOutput>) -> Self {
        Self {
            id: id.into(),
            ok,
            output: output.into(),
        }
    }

    /// A failed result of the call `id`, saying `why`.
    #[must_use]
    pub fn failed(id: impl Into<String>, why: impl Into<String>) -> Self {
        Self::new(id, false, ToolOutput::text(why))
    }
}

/// One message of a conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Message {
    /// Who said it. Tool results are said by the user.
    pub role: Role,
    /// What it is made of, in order.
    pub parts: Vec<Part>,
}

impl Message {
    /// An empty message from `role`.
    #[must_use]
    pub const fn new(role: Role) -> Self {
        Self {
            role,
            parts: Vec::new(),
        }
    }

    /// An empty message from the person.
    #[must_use]
    pub const fn user() -> Self {
        Self::new(Role::User)
    }

    /// An empty message from the model.
    #[must_use]
    pub const fn assistant() -> Self {
        Self::new(Role::Assistant)
    }

    /// This message with `part` added at the end.
    #[must_use]
    pub fn with(mut self, part: Part) -> Self {
        self.parts.push(part);
        self
    }

    /// This message with `text` added.
    #[must_use]
    pub fn text(self, text: impl Into<String>) -> Self {
        self.with(Part::Text(text.into()))
    }

    /// This message with the tool call `call` added.
    #[must_use]
    pub fn call(self, call: ToolCall) -> Self {
        self.with(Part::ToolCall(call))
    }

    /// This message with the tool result `result` added.
    #[must_use]
    pub fn result(self, result: ToolResult) -> Self {
        self.with(Part::ToolResult(result))
    }

    /// The text parts, joined by newlines.
    #[must_use]
    pub fn text_content(&self) -> String {
        let texts: Vec<&str> = self
            .parts
            .iter()
            .filter_map(|part| match part {
                Part::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        texts.join("\n")
    }

    /// The tool calls it asks for, in order.
    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.parts.iter().filter_map(|part| match part {
            Part::ToolCall(call) => Some(call),
            _ => None,
        })
    }

    /// The tool results it carries, in order.
    pub fn tool_results(&self) -> impl Iterator<Item = &ToolResult> {
        self.parts.iter().filter_map(|part| match part {
            Part::ToolResult(result) => Some(result),
            _ => None,
        })
    }

    /// Whether it carries tool results and nothing a person wrote: the harness speaking
    /// for the tools, not a new request.
    #[must_use]
    pub fn is_tool_results(&self) -> bool {
        self.role == Role::User
            && !self.parts.is_empty()
            && self
                .parts
                .iter()
                .all(|part| matches!(part, Part::ToolResult(_)))
    }

    /// Whether nothing is in it.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::Content;
    use alloc::vec;

    #[test]
    fn a_message_says_what_it_is_made_of() {
        let message = Message::assistant()
            .text("one")
            .with(Part::Thinking {
                text: "hm".into(),
                signature: None,
            })
            .text("two")
            .call(ToolCall::new("a", "read", "{}"))
            .call(ToolCall::new("b", "write", "{}"));

        assert_eq!(message.text_content(), "one\ntwo");
        let calls: Vec<&str> = message.tool_calls().map(|call| call.id.as_str()).collect();
        assert_eq!(calls, ["a", "b"]);
        assert!(!message.is_tool_results());
        assert!(!Message::user().is_tool_results());
        assert!(Message::user().is_empty());
    }

    #[test]
    fn tool_results_are_the_users_and_nothing_else() {
        let image = ToolOutput::new(vec![Content::image("image/png", vec![1, 2])]);
        let results = Message::user()
            .result(ToolResult::new("a", true, image))
            .result(ToolResult::failed("b", "no such file"));

        assert!(results.is_tool_results());
        let failed: Vec<bool> = results.tool_results().map(|result| result.ok).collect();
        assert_eq!(failed, [true, false]);
        assert!(!results.text("and a word").is_tool_results());
    }
}
