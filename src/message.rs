//! Messages as JSON: the payload of a `message` event.
//!
//! The shape is described in [`corduroy_core::message`]. [`encode`] writes it and [`decode`]
//! reads it back. `decode` also reads the shape logs written before the harness had a
//! message type of its own hold (oi-kit's, with `content` where this has `parts`), so those
//! logs can still be resumed and read.

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use corduroy_core::{Content, Message, Part, Role, ToolCall, ToolOutput, ToolResult};
use serde_json::{Map, Value, json};

/// `message` as the JSON value a `message` event holds.
#[must_use]
pub fn encode(message: &Message) -> Value {
    let role = match message.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let parts: Vec<Value> = message.parts.iter().filter_map(encode_part).collect();
    json!({"role": role, "parts": parts})
}

fn encode_part(part: &Part) -> Option<Value> {
    Some(match part {
        Part::Text(text) => json!({"type": "text", "text": text}),
        Part::Image { mime_type, data } => {
            json!({"type": "image", "mime_type": mime_type, "data": BASE64.encode(data)})
        }
        Part::Thinking { text, signature } => {
            let mut thinking = json!({"type": "thinking", "text": text});
            if let (Some(signature), Some(fields)) = (signature, thinking.as_object_mut()) {
                fields.insert("signature".to_owned(), json!(signature));
            }
            thinking
        }
        Part::ToolCall(call) => {
            // The input is a JSON object as text; it is stored as JSON, not as a string.
            let input = serde_json::from_str::<Value>(&call.input)
                .unwrap_or_else(|_| Value::String(call.input.clone()));
            json!({"type": "tool_call", "id": call.id, "name": call.name, "input": input})
        }
        Part::ToolResult(result) => json!({
            "type": "tool_result",
            "id": result.id,
            "ok": result.ok,
            "content": encode_content(&result.output),
        }),
        // A kind of part this version does not know how to write.
        _ => return None,
    })
}

/// The blocks of a tool's output as JSON: `{"type": "text", "text"}` and
/// `{"type": "image", "mime_type", "data"}`, the image in base64.
#[must_use]
pub fn encode_content(output: &ToolOutput) -> Value {
    let blocks: Vec<Value> = output
        .content()
        .iter()
        .map(|block| match block {
            Content::Image { mime_type, data } => {
                json!({"type": "image", "mime_type": mime_type, "data": BASE64.encode(data)})
            }
            text => json!({"type": "text", "text": text.to_string()}),
        })
        .collect();
    Value::Array(blocks)
}

/// Read the blocks [`encode_content`] wrote. A kind of block this version does not know
/// stands as a note that it was there.
///
/// # Errors
///
/// Fails if an image is not base64.
pub fn decode_content(blocks: &[Value]) -> Result<ToolOutput> {
    blocks
        .iter()
        .map(|block| match block.get("type").and_then(Value::as_str) {
            Some("image") => {
                let data = BASE64
                    .decode(text(block, "data"))
                    .context("an image is not base64")?;
                Ok(Content::image(text(block, "mime_type"), data))
            }
            Some(kind) if kind != "text" => Ok(Content::text(format!("[{kind} content]"))),
            _ => Ok(Content::text(text(block, "text"))),
        })
        .collect()
}

/// Read a `message` event's payload.
///
/// # Errors
///
/// Fails if the payload is not JSON, has no role, or holds an image that is not base64.
pub fn decode(payload: &str) -> Result<Message> {
    let value: Value = serde_json::from_str(payload).context("a message is not JSON")?;
    let role = match value.get("role").and_then(Value::as_str) {
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        other => bail!("a message has the role {other:?}"),
    };
    let mut message = Message::new(role);
    if let Some(parts) = value.get("parts").and_then(Value::as_array) {
        for part in parts {
            if let Some(part) = decode_part(part)? {
                message = message.with(part);
            }
        }
    } else if let Some(content) = value.get("content").and_then(Value::as_array) {
        for block in content {
            if let Some(part) = legacy::part(block)? {
                message = message.with(part);
            }
        }
    }
    Ok(message)
}

fn decode_part(part: &Value) -> Result<Option<Part>> {
    Ok(Some(match part.get("type").and_then(Value::as_str) {
        Some("text") => Part::Text(text(part, "text")),
        Some("image") => Part::Image {
            mime_type: text(part, "mime_type"),
            data: BASE64
                .decode(text(part, "data"))
                .context("an image is not base64")?,
        },
        Some("thinking") => Part::Thinking {
            text: text(part, "text"),
            signature: part
                .get("signature")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        Some("tool_call") => Part::ToolCall(ToolCall::new(
            text(part, "id"),
            text(part, "name"),
            input_of(part.get("input")),
        )),
        Some("tool_result") => {
            let blocks = part
                .get("content")
                .and_then(Value::as_array)
                .map_or(&[][..], Vec::as_slice);
            Part::ToolResult(ToolResult::new(
                text(part, "id"),
                part.get("ok").and_then(Value::as_bool) != Some(false),
                decode_content(blocks)?,
            ))
        }
        // A kind of part a later version wrote.
        _ => return Ok(None),
    }))
}

/// A tool call's input as JSON text: an object stays one, anything else becomes `{}`.
fn input_of(input: Option<&Value>) -> String {
    match input {
        Some(Value::String(text)) => text.clone(),
        Some(object @ Value::Object(_)) => object.to_string(),
        _ => Value::Object(Map::new()).to_string(),
    }
}

/// The string at `name` in `value`, or an empty one.
fn text(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The shape logs held before the harness had a message type of its own: oi-kit's.
mod legacy {
    use base64::Engine as _;

    use super::{BASE64, Content, Part, Result, ToolCall, ToolOutput, ToolResult};
    use super::{Value, input_of, text};

    /// The part one block of an oi-kit message's `content` stands for, if it is one this
    /// harness keeps.
    pub(super) fn part(block: &Value) -> Result<Option<Part>> {
        Ok(Some(match block.get("type").and_then(Value::as_str) {
            Some("text") => Part::Text(text(block, "text")),
            Some("image") => Part::Image {
                mime_type: text(block, "mimeType"),
                data: BASE64.decode(text(block, "data")).unwrap_or_default(),
            },
            Some("thinking") => Part::Thinking {
                text: text(block, "thinking"),
                signature: block
                    .get("signature")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            },
            Some("toolRequest") => {
                let call = block.get("toolCall").and_then(|call| call.get("value"));
                let name = call.map_or_else(|| "<malformed>".to_owned(), |call| text(call, "name"));
                let input = input_of(call.and_then(|call| call.get("arguments")));
                Part::ToolCall(ToolCall::new(text(block, "id"), name, input))
            }
            Some("toolResponse") => {
                let result = block.get("toolResult");
                let succeeded =
                    result.and_then(|result| result.get("status")) == Some(&Value::from("success"));
                let value = result.and_then(|result| result.get("value"));
                let failed = value
                    .and_then(|value| value.get("isError"))
                    .and_then(Value::as_bool)
                    == Some(true);
                let blocks = value
                    .and_then(|value| value.get("content"))
                    .and_then(Value::as_array)
                    .map_or(&[][..], Vec::as_slice);
                let output: ToolOutput = blocks
                    .iter()
                    .filter_map(|block| match block.get("type").and_then(Value::as_str) {
                        Some("text") => Some(Content::text(text(block, "text"))),
                        Some("image") => BASE64
                            .decode(text(block, "data"))
                            .ok()
                            .map(|data| Content::image(text(block, "mimeType"), data)),
                        _ => None,
                    })
                    .collect();
                let id = text(block, "id");
                Part::ToolResult(ToolResult::new(id, succeeded && !failed, output))
            }
            _ => return Ok(None),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_round_trips_through_json() {
        let image = ToolOutput::new(vec![
            Content::text("a chart"),
            Content::image("image/png", vec![0, 255, 7]),
        ]);
        let message = Message::assistant()
            .with(Part::Thinking {
                text: "plan".into(),
                signature: Some("sig".into()),
            })
            .text("here")
            .call(ToolCall::new("c1", "read", r#"{"path":"a"}"#))
            .with(Part::Image {
                mime_type: "image/jpeg".into(),
                data: vec![1, 2, 3],
            })
            .result(ToolResult::new("c0", false, image));

        let json = encode(&message);
        assert_eq!(json["parts"][2]["input"], json!({"path": "a"}));
        assert_eq!(decode(&json.to_string()).unwrap(), message);
    }

    #[test]
    fn a_message_oi_kit_wrote_is_read() {
        let asked = r#"{"id":"m1","role":"assistant","created":1,"content":[{"type":"text","text":"on it"},{"type":"toolRequest","id":"call_1","toolCall":{"status":"success","value":{"name":"write","arguments":{"path":"x"}}}}],"metadata":{"userVisible":true,"agentVisible":true}}"#;
        let answered = r#"{"id":"m2","role":"user","created":1,"content":[{"type":"toolResponse","id":"call_1","toolResult":{"status":"success","value":{"content":[{"type":"text","text":"Wrote 3 bytes"}],"isError":false}}}],"metadata":{"userVisible":true,"agentVisible":true}}"#;

        assert_eq!(
            decode(asked).unwrap(),
            Message::assistant().text("on it").call(ToolCall::new(
                "call_1",
                "write",
                r#"{"path":"x"}"#
            ))
        );
        assert_eq!(
            decode(answered).unwrap(),
            Message::user().result(ToolResult::new("call_1", true, "Wrote 3 bytes"))
        );
    }

    #[test]
    fn what_cannot_be_read_is_said_or_left_out() {
        assert!(decode("not json").is_err());
        assert!(decode(r#"{"role":"system","parts":[]}"#).is_err());
        let later = r#"{"role":"user","parts":[{"type":"audio"},{"type":"text","text":"hi"}]}"#;
        assert_eq!(decode(later).unwrap(), Message::user().text("hi"));
        let bad =
            r#"{"role":"user","parts":[{"type":"image","mime_type":"image/png","data":"%%"}]}"#;
        assert!(decode(bad).is_err());
    }
}
