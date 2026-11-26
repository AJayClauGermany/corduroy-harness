//! oi-kit's providers as the harness's: converting messages, tools and usage at the
//! boundary, so that nothing past it depends on oi-kit's shapes.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use corduroy_harness::core::{
    AUDIO, Content, IMAGE, Message, PDF, Part, Role, TEXT, ToolCall, ToolSpec, VIDEO,
};
use corduroy_harness::{
    ModelInfo, ModelReply, ModelRequest, Provider, ProviderError, StopReason, Usage,
};
use oi_kit_provider_types::base::Provider as OiKitProvider;
use oi_kit_provider_types::canonical::Modality;
use oi_kit_provider_types::canonical::maybe_get_canonical_model;
use oi_kit_provider_types::conversation::message::{
    Message as OiKitMessage, MessageContent, ToolRequest,
};
use oi_kit_provider_types::formats::openai::is_openai_responses_model;
use oi_kit_provider_types::model::ModelConfig;
use oi_kit_provider_types::retry::retry_operation;
use oi_kit_provider_types::thinking::ThinkingEffort;
use oi_kit_providers::openai::OPEN_AI_DEFAULT_BASE_PATH;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, JsonObject, Tool};
use serde_json::Value;

use crate::Thinking;

/// What every request to a provider asks for, from its [`ProviderSpec`](crate::ProviderSpec).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Settings {
    /// oi-kit's id for the provider, under which its model catalog lists the provider's
    /// models.
    pub(crate) provider: String,
    /// For a provider speaking `OpenAI`'s protocol, the path its requests go to, as oi-kit
    /// derives it from the base URL. It decides, with the model, which of the two APIs a
    /// request is for (see [`Settings::responses`]).
    pub(crate) openai_path: Option<String>,
    pub(crate) max_tokens: Option<u32>,
    pub(crate) temperature: Option<f32>,
    pub(crate) thinking: Option<Thinking>,
}

impl Settings {
    /// The configuration oi-kit is asked to answer `model` with.
    fn model_config(&self, model: &str) -> ModelConfig {
        let mut config = ModelConfig::new(model)
            .with_max_tokens(
                self.max_tokens
                    .and_then(|tokens| i32::try_from(tokens).ok()),
            )
            .with_default_max_tokens(catalog_max_tokens(&self.provider, model))
            .with_temperature(self.temperature);
        if let Some(thinking) = self.thinking {
            let effort = match thinking {
                Thinking::Off => ThinkingEffort::Off,
                Thinking::Low => ThinkingEffort::Low,
                Thinking::Medium => ThinkingEffort::Medium,
                Thinking::High => ThinkingEffort::High,
                Thinking::Max => ThinkingEffort::Max,
            };
            config = config.with_thinking_effort(effort);
            // Asked for by name, so the model is taken to be one that thinks, whether or not
            // the catalog knows it.
            if thinking != Thinking::Off {
                config.reasoning = Some(true);
            }
        }
        config
    }
}

impl Settings {
    /// Whether a request for `model` goes to the Responses API, by oi-kit's own rule: a path
    /// that names an API decides, and the standard chat-completions path leaves it to the
    /// model's name.
    fn responses(&self, model: &str) -> bool {
        let Some(path) = &self.openai_path else {
            return false;
        };
        let path = path.trim_matches('/').to_ascii_lowercase();
        if path != OPEN_AI_DEFAULT_BASE_PATH {
            if path.ends_with("responses") || path.contains("/responses") {
                return true;
            }
            if path.contains("chat/completions") {
                return false;
            }
        }
        is_openai_responses_model(model)
    }

    /// The configuration and the system prompt oi-kit is given to ask `model`.
    ///
    /// A request to the Responses API is not asked the way oi-kit would by default, which
    /// is a `system` message at the head of the input and always a `max_output_tokens`: an
    /// account paying through a `ChatGPT` plan is refused a request holding either. The
    /// system prompt goes as `instructions`, the API's own field for it, and a limit only
    /// where the provider's entry states one; without one the model's own limit applies,
    /// which is what the default asked for.
    fn asking<'system>(&self, model: &str, system: &'system str) -> (ModelConfig, &'system str) {
        let config = self.model_config(model);
        if !self.responses(model) {
            return (config, system);
        }
        let mut config = config.with_max_tokens(
            self.max_tokens
                .and_then(|tokens| i32::try_from(tokens).ok()),
        );
        if !system.is_empty() {
            config = config.with_merged_request_params(HashMap::from([(
                "instructions".to_owned(),
                Value::String(system.to_owned()),
            )]));
        }
        (config, "")
    }
}

/// The most `model` of `provider` may write in one answer, as oi-kit's model catalog has it.
fn catalog_max_tokens(provider: &str, model: &str) -> Option<i32> {
    let limit = maybe_get_canonical_model(provider, model)?.limit;
    limit
        .output
        .filter(|&output| output < limit.context)
        .and_then(|output| i32::try_from(output).ok())
}

/// An oi-kit provider, speaking the harness's messages.
pub(crate) struct Adapted {
    provider: Arc<dyn OiKitProvider>,
    settings: Settings,
}

impl Adapted {
    /// `provider`, asked with `settings`.
    pub(crate) fn new(provider: Arc<dyn OiKitProvider>, settings: Settings) -> Self {
        Self { provider, settings }
    }
}

#[async_trait]
impl Provider for Adapted {
    async fn complete(&self, request: &ModelRequest) -> Result<ModelReply, ProviderError> {
        let (config, system) = self.settings.asking(&request.model, &request.system);
        let messages: Vec<OiKitMessage> = request.messages.iter().map(to_oi_kit).collect();
        let tools = request
            .tools
            .iter()
            .map(tool_of)
            .collect::<Result<Vec<Tool>, ProviderError>>()?;
        let provider = &self.provider;
        // Rate limits, server errors and dropped connections are tried again, as the
        // provider's own policy says.
        let (message, usage) = retry_operation(&provider.retry_config(), || {
            provider.complete(&config, system, &messages, &tools)
        })
        .await
        .map_err(|error| ProviderError::new(error.to_string()))?;
        if let Some(error) = message.error() {
            return Err(ProviderError::new(error.message.clone()));
        }
        let reply = ModelReply::new(from_oi_kit(&message), usage_of(&usage.usage));
        Ok(match usage.finish_reasons.as_deref().and_then(stop_of) {
            Some(stop) => reply.stopped(stop),
            None => reply,
        })
    }

    fn describe(&self, model: &str) -> ModelInfo {
        describe(&self.settings.provider, model)
    }
}

/// What oi-kit's model catalog says about `model` of `provider`: nothing, for a model it
/// does not list, as for one served on this machine.
fn describe(provider: &str, model: &str) -> ModelInfo {
    let Some(known) = maybe_get_canonical_model(provider, model) else {
        return ModelInfo::unknown();
    };
    let tokens = |count: usize| u64::try_from(count).ok().filter(|&count| count > 0);
    let mut info = ModelInfo::unknown().with_tools(known.tool_call);
    info.context_tokens = tokens(known.limit.context);
    info.output_tokens = known.limit.output.and_then(tokens);
    // What the catalogue lists, in its own words for the kinds, which are ours too. A model
    // it lists with no kinds at all says nothing about them.
    let kinds = |listed: &[Modality]| {
        (!listed.is_empty()).then(|| {
            listed
                .iter()
                .map(|kind| kind_of(*kind).to_owned())
                .collect()
        })
    };
    info.takes = kinds(&known.modalities.input);
    info.returns = kinds(&known.modalities.output);
    info
}

/// The name of a kind of content oi-kit's catalogue lists.
const fn kind_of(kind: Modality) -> &'static str {
    match kind {
        Modality::Text => TEXT,
        Modality::Image => IMAGE,
        Modality::Audio => AUDIO,
        Modality::Video => VIDEO,
        Modality::Pdf => PDF,
    }
}

/// Why the model stopped, from the reasons a provider's API gave, each in that API's own
/// words. The first one known decides; a word no API here uses says nothing.
fn stop_of(reasons: &[String]) -> Option<StopReason> {
    reasons
        .iter()
        .find_map(|reason| match reason.to_ascii_lowercase().as_str() {
            "stop" | "end_turn" | "stop_sequence" | "completed" => Some(StopReason::Finished),
            "length" | "max_tokens" | "max_output_tokens" | "incomplete" => Some(StopReason::Limit),
            "tool_use" | "tool_calls" | "function_call" => Some(StopReason::ToolCalls),
            "refusal" | "content_filter" | "safety" => Some(StopReason::Refused),
            _ => None,
        })
}

/// What oi-kit says a request cost, as the harness counts it.
fn usage_of(usage: &oi_kit_provider_types::conversation::token_usage::Usage) -> Usage {
    let count = |tokens: Option<i64>| tokens.and_then(|tokens| u64::try_from(tokens).ok());
    Usage::new(count(usage.input_tokens), count(usage.output_tokens)).with_cache(
        count(usage.cache_read_input_tokens),
        count(usage.cache_write_input_tokens),
    )
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

    fn settings(provider: &str) -> Settings {
        Settings {
            provider: provider.to_owned(),
            openai_path: None,
            max_tokens: None,
            temperature: None,
            thinking: None,
        }
    }

    #[test]
    fn a_known_model_may_write_as_much_as_the_catalog_says() {
        let known = settings("anthropic").model_config("claude-sonnet-4-5");
        let limit = catalog_max_tokens("anthropic", "claude-sonnet-4-5").unwrap();
        assert!(limit > 4096, "{limit}");
        assert_eq!(known.max_output_tokens(), limit);

        let unknown = settings("openai_compatible").model_config("local");
        assert_eq!(unknown.max_output_tokens(), 4096);
    }

    fn openai(path: &str) -> Settings {
        Settings {
            openai_path: Some(path.to_owned()),
            ..settings("openai")
        }
    }

    #[test]
    fn the_path_and_then_the_model_say_which_api_a_request_is_for() {
        let standard = openai("v1/chat/completions");
        assert!(standard.responses("gpt-6-luna"));
        assert!(standard.responses("gpt-5.6-sol"));
        assert!(!standard.responses("gpt-4o"));
        assert!(openai("v1/responses").responses("local"));
        assert!(!openai("chat/completions").responses("gpt-6-luna"));
        assert!(!openai("api/v4/chat/completions").responses("gpt-6-luna"));
        // Another protocol has no Responses API, whatever the model is called.
        assert!(!settings("anthropic").responses("gpt-6-luna"));
    }

    #[test]
    fn a_responses_request_takes_the_system_prompt_as_instructions_and_no_default_limit() {
        let (config, system) = openai("v1/chat/completions").asking("gpt-6-luna", "Be brief.");
        assert_eq!(system, "");
        assert_eq!(config.request_params.unwrap()["instructions"], "Be brief.");
        assert_eq!(config.max_tokens, None);

        let stated = Settings {
            max_tokens: Some(1000),
            ..openai("v1/chat/completions")
        };
        let (config, _) = stated.asking("gpt-6-luna", "");
        assert_eq!(config.max_tokens, Some(1000));
        assert!(config.request_params.is_none());

        // A chat-completions request is asked as before.
        let (config, system) = openai("v1/chat/completions").asking("gpt-4o", "Be brief.");
        assert_eq!(system, "Be brief.");
        assert!(config.request_params.is_none());
        assert!(config.max_tokens.is_some());
    }

    #[test]
    fn the_agent_file_overrides_the_catalog() {
        let config = Settings {
            max_tokens: Some(1000),
            temperature: Some(0.2),
            thinking: Some(Thinking::High),
            ..settings("anthropic")
        }
        .model_config("claude-sonnet-4-5");
        assert_eq!(config.max_output_tokens(), 1000);
        assert_eq!(config.temperature, Some(0.2));
        assert_eq!(config.thinking_effort(), Some(ThinkingEffort::High));
        assert_eq!(config.reasoning, Some(true));

        let off = Settings {
            thinking: Some(Thinking::Off),
            ..settings("anthropic")
        }
        .model_config("claude-sonnet-4-5");
        assert_eq!(off.thinking_effort(), Some(ThinkingEffort::Off));
        assert_eq!(off.reasoning, None);
    }

    #[test]
    fn cached_tokens_are_counted() {
        let usage =
            oi_kit_provider_types::conversation::token_usage::Usage::new(Some(100), Some(20), None)
                .with_cache_tokens(Some(80), Some(10));
        let counted = usage_of(&usage);
        assert_eq!(counted.input_tokens, Some(100));
        assert_eq!(counted.output_tokens, Some(20));
        assert_eq!(counted.cache_read_tokens, Some(80));
        assert_eq!(counted.cache_write_tokens, Some(10));
    }

    #[test]
    fn a_known_model_describes_itself_and_an_unknown_one_says_nothing() {
        let known = describe("anthropic", "claude-sonnet-4-5");
        assert!(
            known
                .context_tokens
                .is_some_and(|context| context >= 200_000)
        );
        assert!(known.output_tokens.is_some_and(|output| output > 4096));
        assert_eq!(known.tools, Some(true));
        assert_eq!(known.takes(IMAGE), Some(true));
        assert_eq!(known.returns(TEXT), Some(true));
        assert_eq!(known.returns(AUDIO), Some(false));

        assert_eq!(describe("openai_compatible", "local"), ModelInfo::unknown());
    }

    #[test]
    fn each_api_s_word_for_why_the_model_stopped_is_understood() {
        let stop = |reason: &str| stop_of(&[reason.to_owned()]);
        assert_eq!(stop("end_turn"), Some(StopReason::Finished));
        assert_eq!(stop("STOP"), Some(StopReason::Finished));
        assert_eq!(stop("length"), Some(StopReason::Limit));
        assert_eq!(stop("MAX_TOKENS"), Some(StopReason::Limit));
        assert_eq!(stop("tool_use"), Some(StopReason::ToolCalls));
        assert_eq!(stop("content_filter"), Some(StopReason::Refused));
        assert_eq!(stop("something new"), None);
        assert_eq!(stop_of(&[]), None);
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
