//! oi-kit's providers as the harness's: converting messages, tools and usage at the
//! boundary, so that nothing past it depends on oi-kit's shapes.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use corduroy_harness::core::{Content, Message, Part, Role, ToolCall, ToolSpec};
use corduroy_harness::{ModelReply, ModelRequest, Provider, ProviderError, Usage};
use oi_kit_provider_types::base::Provider as OiKitProvider;
use oi_kit_provider_types::conversation::message::{
    Message as OiKitMessage, MessageContent, ToolRequest,
};
use oi_kit_provider_types::model::ModelConfig;
use oi_kit_provider_types::retry::retry_operation;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, JsonObject, Tool};
use serde_json::Value;

/// An oi-kit provider, speaking the harness's messages.
pub(crate) struct Adapted(pub(crate) Arc<dyn OiKitProvider>);

#[async_trait]
impl Provider for Adapted {
    async fn complete(&self, request: &ModelRequest) -> Result<ModelReply, ProviderError> {
        let config = ModelConfig::new(&request.model);
        let messages: Vec<OiKitMessage> = request.messages.iter().map(to_oi_kit).collect();
        let tools = request
            .tools
            .iter()
            .map(tool_of)
            .collect::<Result<Vec<Tool>, ProviderError>>()?;
        let provider = &self.0;
        // Rate limits, server errors and dropped connections are tried again, as the
        // provider's own policy says.
        let (message, usage) = retry_operation(&provider.retry_config(), || {
            provider.complete(&config, &request.system, &messages, &tools)
        })
        .await
        .map_err(|error| ProviderError::new(error.to_string()))?;
        if let Some(error) = message.error() {
            return Err(ProviderError::new(error.message.clone()));
        }
        let count = |tokens: Option<i64>| tokens.and_then(|tokens| u64::try_from(tokens).ok());
        let usage = Usage::new(
            count(usage.usage.input_tokens),
            count(usage.usage.output_tokens),
        );
        Ok(ModelReply::new(from_oi_kit(&message), usage))
    }
}

/// `spec` as the tool an oi-kit provider offers the model.
fn tool_of(spec: &ToolSpec) -> Result<Tool, ProviderError> {
    let schema: JsonObject = serde_json::from_str(&spec.input_schema).map_err(|error| {
        ProviderError::new(format!(
            "tool `{}` has an invalid input schema: {error}",
            spec.name
        ))
    })?;
    Ok(Tool::new(
        spec.name.clone(),
        spec.description.clone(),
        Arc::new(schema),
    ))
}

/// The harness's `message` as oi-kit's.
fn to_oi_kit(message: &Message) -> OiKitMessage {
    let start = match message.role {
        Role::User => OiKitMessage::user(),
        Role::Assistant => OiKitMessage::assistant(),
    };
    message
        .parts
        .iter()
        .fold(start, |converted, part| match part {
            Part::Text(text) => converted.with_text(text),
            Part::Image { mime_type, data } => converted.with_image(BASE64.encode(data), mime_type),
            Part::Thinking { text, signature } => {
                converted.with_thinking(text, signature.clone().unwrap_or_default())
            }
            Part::ToolCall(call) => {
                let mut request = CallToolRequestParams::new(call.name.clone());
                if let Ok(Value::Object(arguments)) = serde_json::from_str::<Value>(&call.input) {
                    request = request.with_arguments(arguments);
                }
                converted.with_tool_request(call.id.clone(), Ok(request))
            }
            Part::ToolResult(result) => {
                let blocks: Vec<ContentBlock> = result
                    .output
                    .content()
                    .iter()
                    .map(|block| match block {
                        Content::Image { mime_type, data } => {
                            ContentBlock::image(BASE64.encode(data), mime_type)
                        }
                        text => ContentBlock::text(text.to_string()),
                    })
                    .collect();
                let answer = if result.ok {
                    CallToolResult::success(blocks)
                } else {
                    CallToolResult::error(blocks)
                };
                converted.with_tool_response(result.id.clone(), Ok(answer))
            }
            // A kind of part a later harness has; the model is not shown it.
            _ => converted,
        })
}

/// oi-kit's `message`, a model's answer, as the harness's.
fn from_oi_kit(message: &OiKitMessage) -> Message {
    let mut converted = Message::assistant();
    for content in &message.content {
        let part = match content {
            MessageContent::Text(text) => Part::Text(text.text.clone()),
            MessageContent::Image(image) => match BASE64.decode(&image.data) {
                Ok(data) => Part::Image {
                    mime_type: image.mime_type.clone(),
                    data,
                },
                Err(_) => continue,
            },
            MessageContent::Thinking(thinking) => Part::Thinking {
                text: thinking.thinking.clone(),
                signature: Some(thinking.signature.clone()).filter(|text| !text.is_empty()),
            },
            MessageContent::ToolRequest(request) => Part::ToolCall(call_of(request)),
            _ => continue,
        };
        converted = converted.with(part);
    }
    converted
}

/// The tool call a model's request is. One that could not be read is still a call, of a
/// tool no one has, so that it is answered and the model hears what was wrong.
fn call_of(request: &ToolRequest) -> ToolCall {
    match &request.tool_call {
        Ok(call) => {
            let input = call.arguments.clone().map_or_else(
                || "{}".to_owned(),
                |arguments| Value::Object(arguments).to_string(),
            );
            ToolCall::new(request.id.clone(), call.name.to_string(), input)
        }
        Err(error) => ToolCall::new(
            request.id.clone(),
            format!("<malformed: {}>", error.message),
            "{}",
        ),
    }
}

/// `message` sent to an oi-kit provider and read back as a model's answer would be.
#[cfg(test)]
fn round_trip(message: &Message) -> Message {
    let mut back = from_oi_kit(&to_oi_kit(message));
    back.role = message.role;
    back
}

#[cfg(test)]
mod tests {
    use corduroy_harness::core::{ToolOutput, ToolResult};

    use super::*;

    #[test]
    fn what_a_model_says_survives_the_boundary() {
        let said = Message::assistant()
            .with(Part::Thinking {
                text: "plan".into(),
                signature: Some("sig".into()),
            })
            .text("I'll look.")
            .call(ToolCall::new("c1", "read", r#"{"path":"a.txt"}"#));

        assert_eq!(round_trip(&said), said);
    }

    #[test]
    fn tool_results_go_to_the_model_with_their_images() {
        let output = ToolOutput::new(vec![
            Content::text("a chart"),
            Content::image("image/png", vec![1, 2, 3]),
        ]);
        let results = Message::user()
            .result(ToolResult::new("c1", true, output))
            .result(ToolResult::failed("c2", "no such file"));

        let sent = to_oi_kit(&results);

        let responses: Vec<&CallToolResult> = sent
            .content
            .iter()
            .filter_map(|content| match content {
                MessageContent::ToolResponse(response) => response.tool_result.as_ref().ok(),
                _ => None,
            })
            .collect();
        assert_eq!(responses.len(), 2);
        assert_eq!(
            responses[0].content,
            [
                ContentBlock::text("a chart"),
                ContentBlock::image("AQID", "image/png")
            ]
        );
        assert_eq!(responses[1].is_error, Some(true));
    }

    #[test]
    fn a_tool_with_a_broken_schema_is_named() {
        let spec = ToolSpec::new("broken", "", "not json");
        let error = tool_of(&spec).unwrap_err().to_string();
        assert!(
            error.contains("tool `broken` has an invalid input schema"),
            "{error}"
        );
    }
}
