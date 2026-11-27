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
use core::time::Duration;
#[cfg(feature = "tokio")]
use std::sync::mpsc;
#[cfg(feature = "tokio")]
use std::thread;

use async_trait::async_trait;
use corduroy_core::{
    Context, ModelInfo, ModelReply, ModelRequest, Part, Plugin, PluginError, Registry, Usage,
};
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
///
/// Its [kind](Self::kind) says which sort of failure it was, where the provider can tell,
/// so that whatever reads it need not go by the wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    why: String,
    kind: ProviderErrorKind,
}

/// The sort of failure a [`ProviderError`] is. More are added as providers can tell them
/// apart; one that is not listed, or that the provider could not tell, is
/// [`Other`](Self::Other).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ProviderErrorKind {
    /// The request held more than the model's context does.
    ContextOverflow,
    /// The provider refused the request for coming too soon after others.
    RateLimit,
    /// The credential was missing or refused.
    Auth,
    /// The model did not finish answering in the time it was given.
    Timeout,
    /// The provider could not be reached, or the connection was lost.
    Unreachable,
    /// Anything else, and whatever the provider could not tell apart.
    #[default]
    Other,
}

impl ProviderErrorKind {
    /// The kind's name, as a `turn/end` event records it: `context-overflow`, `rate-limit`,
    /// `auth`, `timeout`, `unreachable` or `other`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ContextOverflow => "context-overflow",
            Self::RateLimit => "rate-limit",
            Self::Auth => "auth",
            Self::Timeout => "timeout",
            Self::Unreachable => "unreachable",
            Self::Other => "other",
        }
    }

    /// The kind called `name`. A name this harness does not know, as a later one may
    /// write, is [`Other`](Self::Other).
    #[must_use]
    pub fn named(name: &str) -> Self {
        match name {
            "context-overflow" => Self::ContextOverflow,
            "rate-limit" => Self::RateLimit,
            "auth" => Self::Auth,
            "timeout" => Self::Timeout,
            "unreachable" => Self::Unreachable,
            _ => Self::Other,
        }
    }
}

impl fmt::Display for ProviderErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl ProviderError {
    /// An error saying `why`, of no particular [kind](ProviderErrorKind::Other).
    #[must_use]
    pub fn new(why: impl Into<String>) -> Self {
        Self {
            why: why.into(),
            kind: ProviderErrorKind::Other,
        }
    }

    /// The same error, said to be of `kind`.
    #[must_use]
    pub const fn of(mut self, kind: ProviderErrorKind) -> Self {
        self.kind = kind;
        self
    }

    /// Which sort of failure this was.
    #[must_use]
    pub const fn kind(&self) -> ProviderErrorKind {
        self.kind
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.why)
    }
}

impl Error for ProviderError {}

/// What a provider asks of its model beside the request itself: the settings it adds on
/// the way, each `None` where it adds none or does not say.
///
/// The loop records them with the request, so the log says what a model was asked for and
/// not only what it was sent. More may be said later, so it is built with
/// [`new`](Self::new) and its builder methods.
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct Asking {
    /// The most the answer may take, in tokens, as the request says it.
    pub max_tokens: Option<u64>,
    /// The temperature asked for.
    pub temperature: Option<f32>,
    /// How hard the model is asked to think, in the provider's word for it.
    pub thinking: Option<String>,
    /// How long one attempt at the request may take in all.
    pub timeout: Option<Duration>,
}

impl Asking {
    /// Nothing said.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_tokens: None,
            temperature: None,
            thinking: None,
            timeout: None,
        }
    }

    /// The answer may take `tokens` at most.
    #[must_use]
    pub const fn max_tokens(mut self, tokens: u64) -> Self {
        self.max_tokens = Some(tokens);
        self
    }

    /// The temperature asked for is `temperature`.
    #[must_use]
    pub const fn temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// The model is asked to think as hard as `thinking` says.
    #[must_use]
    pub fn thinking(mut self, thinking: impl Into<String>) -> Self {
        self.thinking = Some(thinking.into());
        self
    }

    /// One attempt may take `timeout` in all.
    #[must_use]
    pub const fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

/// One attempt at a request that failed and is about to be made again.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Retry {
    /// Which retry this is of the request, counting from 1.
    pub attempt: u32,
    /// Why the attempt before it failed.
    pub error: ProviderError,
    /// How long is waited before the request is made again.
    pub delay: Duration,
    /// Who makes the request again: [`BY_PROVIDER`] for a provider's own retry.
    pub by: String,
}

/// Who retried, where a provider did so by its own policy. A loop that asks again, or
/// anything else that does, names itself otherwise.
pub const BY_PROVIDER: &str = "provider";

impl Retry {
    /// The `attempt`th retry of a request, by the provider itself, after `error` and a wait
    /// of `delay`.
    #[must_use]
    pub fn new(attempt: u32, error: ProviderError, delay: Duration) -> Self {
        Self {
            attempt,
            error,
            delay,
            by: BY_PROVIDER.to_owned(),
        }
    }

    /// The same retry, made by `by` and not by the provider.
    #[must_use]
    pub fn by(mut self, by: impl Into<String>) -> Self {
        self.by = by.into();
        self
    }
}

/// A piece of an answer, told while the model is still writing it.
///
/// Pieces are a preview of the reply, in the order they were written: put together they
/// say what the reply will, and the reply itself still comes whole from
/// [`Provider::complete_reporting`]. A provider tells them at whatever grain its API gives:
/// a word, a line, or the whole of a part at once.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Piece {
    /// More of the answer's text.
    Text(String),
    /// More of what the model thinks before it answers.
    Thinking(String),
    /// More of a tool call the model is writing.
    Call(CallPiece),
    /// What the request has cost so far, where the provider says before the end.
    Usage(Usage),
}

/// More of a tool call being written: see [`Piece::Call`].
///
/// Pieces with the same `id` are of one call. The first names the tool; each adds to the
/// text of its input. A provider that only learns of a call once it is whole tells it as
/// one piece.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CallPiece {
    /// The call's id, as the reply will have it.
    pub id: String,
    /// The tool's name. Empty in a piece that only adds to the input.
    pub name: String,
    /// More of the input's JSON text.
    pub arguments: String,
}

impl CallPiece {
    /// More of the call `id` of the tool `name`: `arguments` is added to its input.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }
}

/// What a provider tells whoever asked while it answers a request. The harness records what
/// it hears; a provider never writes to the event stream itself.
///
/// Every method has a default that does nothing, so one more thing to tell is an addition.
/// A method is called from inside the provider's own work and must not wait.
pub trait Reports: Send + Sync {
    /// An attempt failed and the request is about to be made again. Whatever
    /// [pieces](Self::piece) the failed attempt told are void: the next attempt writes its
    /// answer from the start.
    fn retry(&self, retry: &Retry) {
        let _: &Retry = retry;
    }

    /// The model wrote `piece` of its answer.
    fn piece(&self, piece: &Piece) {
        let _: &Piece = piece;
    }
}

/// Hears nothing.
impl Reports for () {}

/// A model the loop can ask.
///
/// A provider retries what is worth retrying itself (a rate limit, a dropped connection),
/// saying so through [`Provider::complete_reporting`]; an error it returns ends the run.
/// The loop may stop waiting for an answer, when a person stops the run, so a provider must
/// not rely on being awaited to the end.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Answer `request`.
    ///
    /// # Errors
    ///
    /// Returns a [`ProviderError`] if the model could not be reached or did not answer.
    async fn complete(&self, request: &ModelRequest) -> Result<ModelReply, ProviderError>;

    /// Answer `request`, telling `reports` what happens on the way: each [`Piece`] of the
    /// answer as the model writes it, and each attempt that is made again. This is what
    /// the loop calls, and it is how a provider streams: the pieces are a preview for
    /// whoever watches, and the reply returned is the whole.
    ///
    /// By default the answer is [`complete`](Self::complete)'s, told as one piece for each
    /// of its parts once it is there, so whoever watches is told the same things by a
    /// provider that streams and by one that does not. A provider that streams or retries
    /// implements this and has `complete` call it with `&()`.
    ///
    /// # Errors
    ///
    /// As [`complete`](Self::complete).
    async fn complete_reporting(
        &self,
        request: &ModelRequest,
        reports: &dyn Reports,
    ) -> Result<ModelReply, ProviderError> {
        let reply = self.complete(request).await?;
        for piece in pieces_of(&reply) {
            reports.piece(&piece);
        }
        Ok(reply)
    }

    /// What is known about `model`, by the provider's own name for it: what it takes and how
    /// much it holds. Nothing, unless the provider says; a reader has to cope with that.
    fn describe(&self, model: &str) -> ModelInfo {
        let _: &str = model;
        ModelInfo::unknown()
    }

    /// The settings this provider adds to a request for `model`, by its own name for it:
    /// the limit on the answer, the temperature, the thinking, how long it waits. Nothing,
    /// unless the provider says.
    fn asking(&self, model: &str) -> Asking {
        let _: &str = model;
        Asking::new()
    }
}

/// A whole `reply` as the pieces a provider that streamed it would have told: one for each
/// part that is text, thinking or a tool call.
#[must_use]
pub fn pieces_of(reply: &ModelReply) -> Vec<Piece> {
    reply
        .message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text(text) => Some(Piece::Text(text.clone())),
            Part::Thinking { text, .. } => Some(Piece::Thinking(text.clone())),
            Part::ToolCall(call) => Some(Piece::Call(CallPiece::new(
                call.id.clone(),
                call.name.clone(),
                call.input.clone(),
            ))),
            _ => None,
        })
        .collect()
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

    async fn complete_reporting(
        &self,
        request: &ModelRequest,
        reports: &dyn Reports,
    ) -> Result<ModelReply, ProviderError> {
        self.provider.complete_reporting(request, reports).await
    }

    fn describe(&self, model: &str) -> ModelInfo {
        let reported = self.provider.describe(model);
        match self.described.get(model) {
            Some(stated) => stated.clone().or(reported),
            None => reported,
        }
    }

    fn asking(&self, model: &str) -> Asking {
        self.provider.asking(model)
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
