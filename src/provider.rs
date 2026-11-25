//! The model as a service: providers that plugins supply.
//!
//! The loop does not hold a model of its own. It asks the provider registered under a name
//! (see [`Harness::with_provider`](crate::Harness::with_provider)) in the [`Providers`]
//! registry, looked up on every request, so loading or unloading a provider plugin swaps the
//! model between one step and the next. [`provider`] is such a plugin; with the `tokio`
//! feature it also offers the provider's models to plugins through the core's [`Models`]
//! registry, as `<provider>/<model>`, which is what the `llm` capability of WebAssembly
//! plugins reaches. Which of them a plugin may ask is its grant's to say.
//!
//! A request and its reply are the core's ([`ModelRequest`], [`ModelReply`]): the loop and a
//! plugin fill in the same one. Two ways of asking stay over that one description: a
//! [`Provider`], which is asynchronous and what the loop asks, and the core's
//! [`corduroy_core::Model`], which is called and waited for.
//!
//! A [`Provider`] speaks the core's [`Message`](corduroy_core::Message)s. Whatever a model's API wants is the
//! provider's business: `corduroy-providers` converts for the providers it builds, and
//! [`ScriptedProvider`](crate::scripted::ScriptedProvider) needs no conversion at all.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
#[cfg(feature = "tokio")]
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::error::Error;
use core::fmt;
#[cfg(feature = "tokio")]
use core::time::Duration;
#[cfg(feature = "tokio")]
use std::sync::mpsc;
#[cfg(feature = "tokio")]
use std::thread;

use async_trait::async_trait;
use corduroy_core::{Context, ModelInfo, ModelReply, ModelRequest, Plugin, PluginError, Registry};
#[cfg(feature = "tokio")]
use corduroy_core::{MODELS, Model, ModelError, Models};

/// Name of the service under which the harness provides the [`Providers`] registry.
pub const PROVIDERS: &str = "providers";

/// The registry of every model provider, by name.
pub type Providers = Registry<dyn Provider>;

/// The provider a harness asks unless told otherwise, and the name
/// [`Harness::new`](crate::Harness::new) registers its provider under.
pub const DEFAULT_PROVIDER: &str = "default";

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

    /// What is known about `model`, by the provider's own name for it: what it takes and how
    /// much it holds. Nothing, unless the provider says; a reader has to cope with that.
    fn describe(&self, model: &str) -> ModelInfo {
        let _: &str = model;
        ModelInfo::unknown()
    }
}

/// A plugin that registers a model provider under a name, and offers its models to plugins.
/// See [`provider`].
pub struct ProviderPlugin {
    /// The plugin's own name, which cannot be mistaken for another plugin's.
    name: String,
    /// The name the provider is registered under.
    registered_as: String,
    provider: Arc<dyn Provider>,
    /// What the host says about the provider's models, by the provider's name for each.
    described: BTreeMap<String, ModelInfo>,
}

impl fmt::Debug for ProviderPlugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderPlugin")
            .field("name", &self.name)
            .field("described", &self.described)
            .finish_non_exhaustive()
    }
}

/// A plugin that registers `provider` in the [`Providers`] registry as `name`.
///
/// Unloading it removes the provider, and a loop that asks for it then fails its next
/// request. The plugin itself is called `provider-<name>`.
///
/// With the `tokio` feature the provider's models are also there for plugins to ask, in the
/// core's [`Models`] registry, each as `<name>/<model>`. A plugin asks a model synchronously
/// and the provider answers asynchronously, so each such request runs on a thread and tokio
/// runtime of its own. Nothing here lists the models: a WebAssembly plugin's `llm` grant is
/// what says which of them it may ask.
pub fn provider(name: impl Into<String>, provider: Arc<dyn Provider>) -> ProviderPlugin {
    let registered_as = name.into();
    ProviderPlugin {
        name: format!("provider-{registered_as}"),
        registered_as,
        provider,
        described: BTreeMap::new(),
    }
}

impl ProviderPlugin {
    /// State or correct what is known about `model`, by the provider's own name for it. What
    /// `info` says replaces what the provider reports, and what it leaves unknown is still
    /// the provider's to say. The loop and plugins read the result.
    #[must_use]
    pub fn describing(mut self, model: impl Into<String>, info: ModelInfo) -> Self {
        self.described.insert(model.into(), info);
        self
    }
}

impl Plugin for ProviderPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn inject(&self) -> Vec<String> {
        #[cfg(feature = "tokio")]
        let inject = vec![PROVIDERS.to_owned(), MODELS.to_owned()];
        #[cfg(not(feature = "tokio"))]
        let inject = vec![PROVIDERS.to_owned()];
        inject
    }

    fn apply(&mut self, ctx: &mut Context<'_>) -> Result<(), PluginError> {
        let provider: Arc<dyn Provider> = if self.described.is_empty() {
            Arc::clone(&self.provider)
        } else {
            Arc::new(Described {
                provider: Arc::clone(&self.provider),
                described: self.described.clone(),
            })
        };
        let providers = ctx.service::<Providers>(PROVIDERS)?;
        providers.register(ctx, self.registered_as.clone(), Arc::clone(&provider))?;
        #[cfg(feature = "tokio")]
        {
            let models = ctx.service::<Models>(MODELS)?;
            let bridged: Arc<dyn Model> = Arc::new(Bridged { provider });
            models.register(ctx, self.registered_as.clone(), bridged)?;
        }
        Ok(())
    }
}

/// A provider with what the host says about some of its models laid over what it reports.
struct Described {
    provider: Arc<dyn Provider>,
    described: BTreeMap<String, ModelInfo>,
}

#[async_trait]
impl Provider for Described {
    async fn complete(&self, request: &ModelRequest) -> Result<ModelReply, ProviderError> {
        self.provider.complete(request).await
    }

    fn describe(&self, model: &str) -> ModelInfo {
        let reported = self.provider.describe(model);
        self.described
            .get(model)
            .map_or(reported, |stated| stated.or(reported))
    }
}

/// A provider's models, asked synchronously.
#[cfg(feature = "tokio")]
struct Bridged {
    provider: Arc<dyn Provider>,
}

#[cfg(feature = "tokio")]
impl Model for Bridged {
    fn complete(
        &self,
        request: &ModelRequest,
        timeout: Duration,
    ) -> Result<ModelReply, ModelError> {
        let request = request.clone();
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
                        .map_err(|error| ModelError::Failed(error.to_string()))
                });
            // The caller may have stopped waiting.
            drop(answer.send(outcome));
        });
        answered
            .recv_timeout(timeout)
            .map_err(|_| ModelError::TimedOut(timeout))?
    }

    fn describe(&self, model: &str) -> ModelInfo {
        self.provider.describe(model)
    }
}
