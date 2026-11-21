//! Plugin tools, offered to the model through oi-kit.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use corduroy_core::{ToolError, Tools};
use oi_kit_agent::operation::{Emitter, Operation};
use oi_kit_agent::tool::ToolProvider;
use oi_kit_provider_types::conversation::Conversation;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, JsonObject, Tool,
};
use serde_json::Value;

use crate::session::{Effect, Session};

/// Exposes whatever is in the [`Tools`] registry right now.
///
/// The registry is read before every model request and again before every tool call, so
/// loading or unloading a plugin changes what the model can do from the next step on.
#[derive(Debug)]
pub struct PluginTools {
    tools: Arc<Tools>,
}

impl PluginTools {
    /// Serve the tools registered in `tools`.
    #[must_use]
    pub const fn new(tools: Arc<Tools>) -> Self {
        Self { tools }
    }
}

#[async_trait]
impl ToolProvider<Session> for PluginTools {
    async fn tools(&self, _session: &Session) -> Result<Vec<Tool>> {
        self.tools
            .specs()
            .into_iter()
            .map(|spec| {
                let schema: JsonObject = serde_json::from_str(&spec.input_schema)
                    .with_context(|| format!("tool `{}` has an invalid input schema", spec.name))?;
                Ok(Tool::new(spec.name, spec.description, Arc::new(schema)))
            })
            .collect()
    }

    async fn call(
        &self,
        _session: &Session,
        _request_id: &str,
        call: CallToolRequestParams,
        _emit: &Emitter,
    ) -> Result<CallToolResult, ErrorData> {
        let tools = Arc::clone(&self.tools);
        let name = call.name.to_string();
        let input = Value::Object(call.arguments.unwrap_or_default()).to_string();
        // Tools are synchronous and may run for a while (a shell command, say).
        let outcome = tokio::task::spawn_blocking(move || tools.call(&name, &input))
            .await
            .unwrap_or_else(|error| Err(ToolError::Failed(format!("the tool panicked: {error}"))));
        // A failed tool is data for the model, not an error for the run.
        Ok(match outcome {
            Ok(output) => CallToolResult::success(vec![ContentBlock::text(output)]),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error.to_string())]),
        })
    }
}

/// Adds the base system prompt and every plugin's prompt part to each model request.
#[derive(Debug)]
pub struct PromptOperation {
    system: String,
    prompts: Arc<corduroy_core::Prompts>,
}

impl PromptOperation {
    /// Contribute `system` followed by the parts registered in `prompts`.
    #[must_use]
    pub const fn new(system: String, prompts: Arc<corduroy_core::Prompts>) -> Self {
        Self { system, prompts }
    }
}

#[async_trait]
impl Operation<Session, Effect> for PromptOperation {
    fn name(&self) -> &'static str {
        "prompt"
    }

    async fn prompt_parts(
        &self,
        _session: &Session,
        _conversation: &Conversation,
    ) -> Result<Vec<(String, String)>> {
        let mut parts = Vec::new();
        if !self.system.is_empty() {
            parts.push(("system".to_owned(), self.system.clone()));
        }
        parts.extend(self.prompts.render());
        Ok(parts)
    }
}
