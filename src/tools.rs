//! Plugin tools, offered to the model through oi-kit.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use corduroy_core::{SessionId, ToolError, Tools};
use oi_kit_agent::machine::MachineSession as _;
use oi_kit_agent::operation::{Emitter, Operation};
use oi_kit_agent::tool::ToolProvider;
use oi_kit_provider_types::conversation::Conversation;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, JsonObject, Tool,
};
use serde_json::Value;

use crate::session::{Effect, Session, Sessions};

/// Exposes whatever is in the [`Tools`] registry right now.
///
/// The registry is read before every model request and again before every tool call, so
/// loading or unloading a plugin changes what the model can do from the next step on.
///
/// oi-kit's tools step runs every call of a model message and hands back all the results
/// at once. Each call passes through here on its way, so this is where a call gets a trail
/// of its own in the log: a `tool/start` stored before the tool runs and a `tool/result`
/// stored as soon as it returns.
#[derive(Debug)]
pub struct PluginTools {
    tools: Arc<Tools>,
    sessions: Arc<Sessions>,
}

impl PluginTools {
    /// Serve the tools registered in `tools`, journaling each call to its session's journal
    /// in `sessions`.
    #[must_use]
    pub const fn new(tools: Arc<Tools>, sessions: Arc<Sessions>) -> Self {
        Self { tools, sessions }
    }
}

/// A journal that cannot be reached or written to. The tool is not run, and the step that
/// carries this back fails on the same log.
fn unrecorded(error: &dyn std::fmt::Display) -> ErrorData {
    ErrorData::internal_error(
        format!("the tool call could not be recorded: {error}"),
        None,
    )
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
        session: &Session,
        request_id: &str,
        call: CallToolRequestParams,
        _emit: &Emitter,
    ) -> Result<CallToolResult, ErrorData> {
        let journal = self
            .sessions
            .journal(&SessionId::new(session.id()))
            .map_err(|error| unrecorded(&error))?;
        // Intent first: if this cannot be stored, the tool must not run.
        journal
            .start_call(request_id)
            .map_err(|error| unrecorded(&error))?;
        let tools = Arc::clone(&self.tools);
        let name = call.name.to_string();
        let input = Value::Object(call.arguments.unwrap_or_default()).to_string();
        // Tools are synchronous and may run for a while (a shell command, say).
        let outcome = tokio::task::spawn_blocking(move || tools.call(&name, &input))
            .await
            .unwrap_or_else(|error| Err(ToolError::Failed(format!("the tool panicked: {error}"))));
        // A failed tool is data for the model, not an error for the run.
        let (ok, output) = match outcome {
            Ok(output) => (true, output),
            Err(error) => (false, error.to_string()),
        };
        journal
            .finish_call(request_id, ok, &output)
            .map_err(|error| unrecorded(&error))?;
        let content = vec![ContentBlock::text(output)];
        Ok(if ok {
            CallToolResult::success(content)
        } else {
            CallToolResult::error(content)
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
