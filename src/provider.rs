//! The model as a service: providers that plugins supply.
//!
//! The loop does not hold a model of its own. It asks the provider registered under a name
//! (see [`Harness::with_provider`](crate::Harness::with_provider)) in the [`Providers`]
//! registry, looked up on every request, so loading or unloading a provider plugin swaps the
//! model between one step and the next. [`provider`] is such a plugin; it can also offer
//! named models to plugins through the core's [`Models`] registry, which is what the `llm`
//! capability of WebAssembly plugins reaches.

use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use async_trait::async_trait;
use corduroy_core::{
    Completion, Context, MODELS, Model, ModelError, Models, Plugin, PluginError, Registry, Role,
};
use oi_kit_provider_types::base::{MessageStream, ModelInfo, Provider};
use oi_kit_provider_types::conversation::message::Message;
use oi_kit_provider_types::errors::ProviderError;
use oi_kit_provider_types::model::ModelConfig;
use oi_kit_provider_types::retry::RetryConfig;
use rmcp::model::Tool;

/// Name of the service under which the harness provides the [`Providers`] registry.
pub const PROVIDERS: &str = "providers";

/// The registry of every model provider, by name.
pub type Providers = Registry<dyn Provider>;

/// The provider a harness asks unless told otherwise, and the name
/// [`Harness::new`](crate::Harness::new) registers its provider under.
pub const DEFAULT_PROVIDER: &str = "default";

/// A plugin that registers a model provider under a name, and optionally offers some of its
/// models to plugins. See [`provider`].
pub struct ProviderPlugin {
    /// The name the provider is registered under.
    name: String,
    /// The plugin's own name, which cannot be mistaken for another plugin's.
    plugin: String,
    provider: Arc<dyn Provider>,
    models: Vec<String>,
}

impl std::fmt::Debug for ProviderPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderPlugin")
            .field("name", &self.name)
            .field("models", &self.models)
            .finish_non_exhaustive()
    }
}

/// A plugin that registers `provider` in the [`Providers`] registry as `name`. Unloading it
/// removes the provider, and a loop that asks for it then fails its next request. The
/// plugin itself is called `provider-<name>`.
pub fn provider(name: impl Into<String>, provider: Arc<dyn Provider>) -> ProviderPlugin {
    let name = name.into();
    ProviderPlugin {
        plugin: format!("provider-{name}"),
        name,
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
        &self.plugin
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
        providers.register(ctx, self.name.clone(), Arc::clone(&self.provider))?;
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
                Role::User => Message::user().with_text(&turn.text),
                Role::Assistant => Message::assistant().with_text(&turn.text),
            })
            .collect();
        let system = request.system.clone();
        let config = ModelConfig::new(&self.model);
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
                        .block_on(provider.complete(&config, &system, &messages, &[]))
                        .map(|(message, _usage)| message.as_concat_text())
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

/// The provider the loop talks to: whichever is registered under `name` when it is asked.
///
/// It reports `name` as its own, so oi-kit takes everything registered under one name for
/// one provider: signed thinking and provider sessions are kept across a swap. Register a
/// different kind of provider under a different name and point the harness at it.
pub(crate) struct Routed {
    pub(crate) providers: Arc<Providers>,
    pub(crate) name: String,
}

impl Routed {
    fn current(&self) -> Result<Arc<dyn Provider>, ProviderError> {
        self.providers.get(&self.name).ok_or_else(|| {
            ProviderError::ExecutionError(format!(
                "no model provider is registered as `{}`",
                self.name
            ))
        })
    }
}

#[async_trait]
impl Provider for Routed {
    fn get_name(&self) -> &str {
        &self.name
    }

    fn provider_session_id(&self) -> Option<String> {
        self.current().ok()?.provider_session_id()
    }

    async fn resume(&self, session_id: &str) -> Result<(), ProviderError> {
        self.current()?.resume(session_id).await
    }

    async fn stream(
        &self,
        model_config: &ModelConfig,
        system: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<MessageStream, ProviderError> {
        self.current()?
            .stream(model_config, system, messages, tools)
            .await
    }

    fn retry_config(&self) -> RetryConfig {
        self.current().map_or_else(
            |_| RetryConfig::default(),
            |provider| provider.retry_config(),
        )
    }

    async fn fetch_model_info(&self, model_name: &str) -> Result<ModelInfo, ProviderError> {
        self.current()?.fetch_model_info(model_name).await
    }
}
