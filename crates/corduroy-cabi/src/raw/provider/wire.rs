//! A model request and its reply as text, which is how they cross into C.
//!
//! Neither has a text form anywhere else: the log writes messages one at a time, and the
//! WebAssembly world passes records. This one is made up for the prototype, around the
//! `message` event's JSON.

use corduroy_core::{ModelReply, ModelRequest, StopReason, Usage};
use corduroy_harness::message;
use serde_json::{Value, json};

/// `request` as JSON: `model`, `system`, `messages` (each as a `message` event holds it)
/// and `tools` (`name`, `description`, `input_schema`).
pub(super) fn request(request: &ModelRequest) -> String {
    let messages: Vec<Value> = request.messages.iter().map(message::encode).collect();
    let tools: Vec<Value> = request
        .tools
        .iter()
        .map(|tool| {
            let schema = serde_json::from_str::<Value>(&tool.input_schema)
                .unwrap_or_else(|_| Value::String(tool.input_schema.clone()));
            json!({"name": tool.name, "description": tool.description, "input_schema": schema})
        })
        .collect();
    json!({
        "model": request.model,
        "system": request.system,
        "messages": messages,
        "tools": tools,
    })
    .to_string()
}

/// A reply from JSON: `message`, and optionally `usage` (`input_tokens`, `output_tokens`)
/// and `stop`.
pub(super) fn reply(text: &str) -> Result<ModelReply, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|error| format!("a reply is not JSON: {error}"))?;
    let said = value.get("message").ok_or("a reply has no `message`")?;
    let said = message::decode(&said.to_string()).map_err(|error| error.to_string())?;
    let count = |key: &str| {
        value
            .get("usage")
            .and_then(|usage| usage.get(key))
            .and_then(Value::as_u64)
    };
    let reply = ModelReply::new(
        said,
        Usage::new(count("input_tokens"), count("output_tokens")),
    );
    let stop = value
        .get("stop")
        .and_then(Value::as_str)
        .and_then(StopReason::parse);
    Ok(match stop {
        Some(reason) => reply.stopped(reason),
        None => reply,
    })
}
