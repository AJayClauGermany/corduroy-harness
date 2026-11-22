//! The model as a service: providers that plugins supply.
//!
//! The loop does not hold a model of its own. It asks the provider registered under a name
//! (see [`Harness::with_provider`](crate::Harness::with_provider)) in the [`Providers`]
//! registry, looked up on every request, so loading or unloading a provider plugin swaps the
//! model between one step and the next. [`provider`] is such a plugin; it can also offer
//! named models to plugins through the core's [`Models`] registry, which is what the `llm`
//! capability of WebAssembly plugins reaches.
//!
//! A [`Provider`] speaks the harness's own [`Message`]s. Whatever a model's API wants is the
//! provider's business: `corduroy-providers` converts for the providers it builds, and
//! [`ScriptedProvider`](crate::scripted::ScriptedProvider) needs no conversion at all.

use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use async_trait::async_trait;
use corduroy_core::{
    Completion, Context, MODELS, Message, Model, ModelError, Models, Plugin, PluginError, Registry,
    Role, ToolSpec,
};

/// Name of the service under which the harness provides the [`Providers`] registry.
pub const PROVIDERS: &str = "providers";

/// The registry of every model provider, by name.
pub type Providers = Registry<dyn Provider>;

/// The provider a harness asks unless told otherwise, and the name
/// [`Harness::new`](crate::Harness::new) registers its provider under.
pub const DEFAULT_PROVIDER: &str = "default";

/// One request to a model: what the loop asks at each turn.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelRequest {
    /// The model to ask, by the provider's name for it.
    pub model: String,
    /// The system prompt.
    pub system: String,
    /// The conversation as the model sees it, oldest first. It ends with the user (a request
    /// or tool results), and roles alternate.
    pub messages: Vec<Message>,
    /// The tools the model may call.
    pub tools: Vec<ToolSpec>,
}

impl ModelRequest {
    /// A request to `model` with `system` and `messages`, and no tools.
    #[must_use]
    pub fn new(
        model: impl Into<String>,
        system: impl Into<String>,
        messages: Vec<Message>,
    ) -> Self {
        Self {
            model: model.into(),
            system: system.into(),
            messages,
            tools: Vec::new(),
        }
    }

    /// Offer the model `tools`.
    #[must_use]
    pub fn with_tools(mut self, tools: Vec<ToolSpec>) -> Self {
        self.tools = tools;
        self
    }
}

/// What a model answered.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelReply {
    /// The answer: text, thinking and tool calls, from the assistant.
    pub message: Message,
    /// What the request cost.
    pub usage: Usage,
}

impl ModelReply {
    /// The answer `message`, at a cost of `usage`.
    #[must_use]
    pub const fn new(message: Message, usage: Usage) -> Self {
        Self { message, usage }
    }
}

/// What one model request cost, as far as the provider says.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Usage {
    /// Tokens read: the system prompt, the conversation and the tools.
    pub input_tokens: Option<u64>,
    /// Tokens written.
    pub output_tokens: Option<u64>,
}

impl Usage {
    /// `input` tokens read and `output` tokens written.
    #[must_use]
    pub const fn new(input: Option<u64>, output: Option<u64>) -> Self {
        Self {
            input_tokens: input,
            output_tokens: output,
        }
    }
}

/// Why a model gave no answer. The run that asked ends as failed, with this as its reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError(String);

impl ProviderError {
    /// An error saying `why`.
    #[must_use]
    pub fn new(why: impl Into<String>) -> Self {
        Self(why.into())
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for ProviderError {}

/// A model the loop can ask.
///
/// A provider retries what is worth retrying itself (a rate limit, a dropped connection);
/// an error it returns ends the run. The loop may stop waiting for an answer, when a person
/// stops the run, so a provider must not rely on being awaited to the end.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Answer `request`.
    ///
    /// # Errors
    ///
    /// Returns a [`ProviderError`] if the model could not be reached or did not answer.
    async fn complete(&self, request: &ModelRequest) -> Result<ModelReply, ProviderError>;
}

/// A plugin that registers a model provider under a name, and optionally offers some of its
/// models to plugins. See [`provider`].
pub struct ProviderPlugin {
    /// The plugin's own name, which cannot be mistaken for another plugin's.
    name: String,
    /// The name the provider is registered under.
    registered_as: String,
    provider: Arc<dyn Provider>,
    models: Vec<String>,
}

impl fmt::Debug for ProviderPlugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderPlugin")
            .field("name", &self.name)
            .field("models", &self.models)
            .finish_non_exhaustive()
    }
}

/// A plugin that registers `provider` in the [`Providers`] registry as `name`.
///
/// Unloading it removes the provider, and a loop that asks for it then fails its next
/// request. The plugin itself is called `provider-<name>`.
pub fn provider(name: impl Into<String>, provider: Arc<dyn Provider>) -> ProviderPlugin {
    let registered_as = name.into();
    ProviderPlugin {
        name: format!("provider-{registered_as}"),
        registered_as,
        provider,
        models: Vec::new(),
    }
}

impl ProviderPlugin {
    /// Also offer these models of the provider to plugins, each registered under its own name
    /// in the core's [`Models`] registry.
    #[must_use]
    pub fn serving<S: Into<String>>(mut self, models: impl IntoIterator<Item = S>) -> Self {
        self.models.extend(models.into_iter().map(Into::into));
        self
    }
}

impl Plugin for ProviderPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn inject(&self) -> Vec<String> {
        let mut inject = vec![PROVIDERS.to_owned()];
        if !self.models.is_empty() {
            inject.push(MODELS.to_owned());
        }
        inject
    }

    fn apply(&mut self, ctx: &mut Context<'_>) -> Result<(), PluginError> {
        let providers = ctx.service::<Providers>(PROVIDERS)?;
        providers.register(ctx, self.registered_as.clone(), Arc::clone(&self.provider))?;
        if self.models.is_empty() {
            return Ok(());
        }
        let models = ctx.service::<Models>(MODELS)?;
        for model in &self.models {
            let bridged: Arc<dyn Model> = Arc::new(Bridged {
                provider: Arc::clone(&self.provider),
                model: model.clone(),
            });
            models.register(ctx, model.clone(), bridged)?;
        }
        Ok(())
    }
}

/// One model of a provider, asked synchronously.
struct Bridged {
    provider: Arc<dyn Provider>,
    model: String,
}

impl Model for Bridged {
    fn complete(&self, request: &Completion, timeout: Duration) -> Result<String, ModelError> {
        let messages: Vec<Message> = request
            .messages
            .iter()
            .map(|turn| match turn.role {
                Role::User => Message::user().text(&turn.text),
                Role::Assistant => Message::assistant().text(&turn.text),
            })
            .collect();
        let request = ModelRequest::new(&self.model, &request.system, messages);
        let provider = Arc::clone(&self.provider);
        let (answer, answered) = mpsc::channel();
        // Plugins are called from blocking threads and from inside the loop's runtime alike,
        // so the request runs on a thread and runtime of its own. A request that outlives
        // `timeout` is abandoned; its thread ends when the provider returns.
        thread::spawn(move || {
            let outcome = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| ModelError::Failed(error.to_string()))
                .and_then(|runtime| {
                    runtime
                        .block_on(provider.complete(&request))
                        .map(|reply| reply.message.text_content())
                        .map_err(|error| ModelError::Failed(error.to_string()))
                });
            // The caller may have stopped waiting.
            drop(answer.send(outcome));
        });
        answered
            .recv_timeout(timeout)
            .map_err(|_| ModelError::TimedOut(timeout))?
    }
}
