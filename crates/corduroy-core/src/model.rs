//! Models: what a model call is, and the models plugins can ask.
//!
//! Whatever asks a model fills in the same [`ModelRequest`] and reads the same
//! [`ModelReply`]: the agent loop through the harness's provider, and a plugin through the
//! [`Models`] registry, which the host provides as the service named [`MODELS`] and the `llm`
//! capability of WebAssembly plugins reaches. A reply says what the model answered, what the
//! request cost ([`Usage`]) and why the model stopped ([`StopReason`]), each as far as the
//! model's provider says.
//!
//! An entry of the registry answers for every model under one name, usually a provider's, and
//! a model is asked for as `<provider>/<model>`: the entry's name, then that entry's own name
//! for the model, divided at the first `/`. A model also describes itself ([`ModelInfo`]):
//! what it takes and how much it holds, where every field may be unknown.
//!
//! The core only says what a request and its answer are; whatever registers a model (in
//! `corduroy-harness`, a provider plugin) does the talking.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;
use core::error::Error;
use core::fmt;
use core::time::Duration;

use crate::message::Message;
use crate::registry::Registry;
use crate::tool::ToolSpec;

/// Name of the service under which the host provides the [`Models`] registry.
pub const MODELS: &str = "models";

/// The registry of what plugins may ask a model of: each entry answers for the models under
/// its name, asked for as `<name>/<model>`.
pub type Models = Registry<dyn Model>;

/// One request to a model. The agent loop and a plugin fill in the same one.
///
/// More may be said in a request later, so it is built with [`new`](Self::new) and its
/// builder methods.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelRequest {
    /// The model to ask. Given to the [`Models`] registry it is `<provider>/<model>`; what a
    /// [`Model`] or a provider receives is its own name for the model.
    pub model: String,
    /// The system prompt.
    pub system: String,
    /// The conversation as the model sees it, oldest first. It ends with the user (a request
    /// or tool results), and roles alternate.
    pub messages: Vec<Message>,
    /// The tools the model may call. Only the agent loop offers any: a model a plugin asks
    /// cannot act.
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
    /// The answer, from the assistant: text, thinking and tool calls.
    pub message: Message,
    /// What the request cost.
    pub usage: Usage,
    /// Why the model stopped, if its provider said.
    pub stop: Option<StopReason>,
}

impl ModelReply {
    /// The answer `message`, at a cost of `usage`, from a provider that did not say why the
    /// model stopped.
    #[must_use]
    pub const fn new(message: Message, usage: Usage) -> Self {
        Self {
            message,
            usage,
            stop: None,
        }
    }

    /// The same, from a model that stopped because of `reason`.
    #[must_use]
    pub const fn stopped(mut self, reason: StopReason) -> Self {
        self.stop = Some(reason);
        self
    }
}

/// Why a model stopped writing.
///
/// More reasons may follow, so code that tells them apart needs an arm for the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StopReason {
    /// It had said what it meant to.
    Finished,
    /// It reached the most it may write, so the answer is cut off.
    Limit,
    /// It asked for tools to be called.
    ToolCalls,
    /// It, or its provider, declined to answer.
    Refused,
}

impl StopReason {
    /// The reason the log writes as `text`, or `None` for one this version does not know.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "finished" => Some(Self::Finished),
            "limit" => Some(Self::Limit),
            "tool_calls" => Some(Self::ToolCalls),
            "refused" => Some(Self::Refused),
            _ => None,
        }
    }

    /// The reason as the log writes it: `finished`, `limit`, `tool_calls` or `refused`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::Limit => "limit",
            Self::ToolCalls => "tool_calls",
            Self::Refused => "refused",
        }
    }
}

impl fmt::Display for StopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What one model request cost, as far as the provider says.
///
/// A count that is absent means the provider did not say; zero means it said none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
#[expect(
    clippy::struct_field_names,
    reason = "each field is a count of tokens, named as the log's keys are"
)]
pub struct Usage {
    /// Tokens read: the system prompt, the conversation and the tools.
    pub input_tokens: Option<u64>,
    /// Tokens written.
    pub output_tokens: Option<u64>,
    /// Of the tokens read, those served from the provider's prompt cache.
    pub cache_read_tokens: Option<u64>,
    /// Of the tokens read, those written to the provider's prompt cache.
    pub cache_write_tokens: Option<u64>,
    /// Of the tokens written, those spent on reasoning before the answer.
    pub reasoning_tokens: Option<u64>,
}

impl Usage {
    /// `input` tokens read and `output` tokens written.
    #[must_use]
    pub const fn new(input: Option<u64>, output: Option<u64>) -> Self {
        Self {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
        }
    }

    /// The same, with `read` of the tokens read served from the prompt cache and `written`
    /// of them written to it.
    #[must_use]
    pub const fn with_cache(mut self, read: Option<u64>, written: Option<u64>) -> Self {
        self.cache_read_tokens = read;
        self.cache_write_tokens = written;
        self
    }

    /// The same, with `reasoning` of the tokens written spent on reasoning.
    #[must_use]
    pub const fn with_reasoning(mut self, reasoning: Option<u64>) -> Self {
        self.reasoning_tokens = reasoning;
        self
    }

    /// Each count the provider gave, under the key the log writes it as, in a fixed order.
    /// Whatever records a model call writes these and leaves out the rest.
    pub fn counts(&self) -> impl Iterator<Item = (&'static str, u64)> {
        [
            ("input_tokens", self.input_tokens),
            ("output_tokens", self.output_tokens),
            ("cache_read_tokens", self.cache_read_tokens),
            ("cache_write_tokens", self.cache_write_tokens),
            ("reasoning_tokens", self.reasoning_tokens),
        ]
        .into_iter()
        .filter_map(|(key, count)| count.map(|count| (key, count)))
    }
}

/// What a model says about itself. Every field may be unknown: a model served on this
/// machine often reports nothing, and whatever reads a description has to cope with that.
///
/// What a model takes and returns are lists of kinds of content, by name: `text`, `image`,
/// `audio`, `video`, `pdf`, or a kind the core has never heard of. The core does not list
/// them, so a model of a new kind (an embedding, a yes or a no) needs no change here.
///
/// More may be described later, so it is built from [`unknown`](Self::unknown) with its
/// builder methods.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelInfo {
    /// How many tokens the model holds at once: everything it reads and writes in a request.
    pub context_tokens: Option<u64>,
    /// The most tokens it may write in one answer.
    pub output_tokens: Option<u64>,
    /// The kinds of content it takes, such as `text` and `image`.
    pub takes: Option<Vec<String>>,
    /// The kinds of content it returns, such as `text`.
    pub returns: Option<Vec<String>>,
    /// Whether it can be offered tools.
    pub tools: Option<bool>,
}

/// Content of text.
pub const TEXT: &str = "text";
/// An image.
pub const IMAGE: &str = "image";
/// Sound.
pub const AUDIO: &str = "audio";
/// Moving pictures.
pub const VIDEO: &str = "video";
/// A PDF document.
pub const PDF: &str = "pdf";

impl ModelInfo {
    /// A description that knows nothing.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            context_tokens: None,
            output_tokens: None,
            takes: None,
            returns: None,
            tools: None,
        }
    }

    /// The same, of a model that holds `tokens` at once.
    #[must_use]
    pub const fn with_context(mut self, tokens: u64) -> Self {
        self.context_tokens = Some(tokens);
        self
    }

    /// The same, of a model that may write `tokens` in one answer.
    #[must_use]
    pub const fn with_output(mut self, tokens: u64) -> Self {
        self.output_tokens = Some(tokens);
        self
    }

    /// The same, of a model that takes `kinds` of content and no others.
    #[must_use]
    pub fn taking<S: Into<String>>(mut self, kinds: impl IntoIterator<Item = S>) -> Self {
        self.takes = Some(kinds.into_iter().map(Into::into).collect());
        self
    }

    /// The same, of a model that returns `kinds` of content and no others.
    #[must_use]
    pub fn returning<S: Into<String>>(mut self, kinds: impl IntoIterator<Item = S>) -> Self {
        self.returns = Some(kinds.into_iter().map(Into::into).collect());
        self
    }

    /// The same, of a model that can be offered tools, or cannot.
    #[must_use]
    pub const fn with_tools(mut self, takes: bool) -> Self {
        self.tools = Some(takes);
        self
    }

    /// Whether the model takes content of `kind`, or `None` if that is not known.
    #[must_use]
    pub fn takes(&self, kind: &str) -> Option<bool> {
        self.takes
            .as_ref()
            .map(|kinds| kinds.iter().any(|taken| taken == kind))
    }

    /// Whether the model returns content of `kind`, or `None` if that is not known.
    #[must_use]
    pub fn returns(&self, kind: &str) -> Option<bool> {
        self.returns
            .as_ref()
            .map(|kinds| kinds.iter().any(|returned| returned == kind))
    }

    /// This description, with what it does not know taken from `other`. It is how a
    /// correction is laid over what a provider reports: a list it states replaces the
    /// provider's whole.
    #[must_use]
    pub fn or(self, other: Self) -> Self {
        Self {
            context_tokens: self.context_tokens.or(other.context_tokens),
            output_tokens: self.output_tokens.or(other.output_tokens),
            takes: self.takes.or(other.takes),
            returns: self.returns.or(other.returns),
            tools: self.tools.or(other.tools),
        }
    }
}

/// Why a model gave no answer.
///
/// More cases may follow, so code that tells them apart needs an arm for the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModelError {
    /// No model is registered under this name.
    Unknown(String),
    /// The model was asked and failed.
    Failed(String),
    /// The model did not answer in time.
    TimedOut(Duration),
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(name) => write!(
                f,
                "unknown model `{name}`: a model is named `<provider>/<model>`, after a \
                 provider that serves it"
            ),
            Self::Failed(reason) => write!(f, "the model failed: {reason}"),
            Self::TimedOut(limit) => {
                write!(f, "the model did not answer within {}ms", limit.as_millis())
            }
        }
    }
}

impl Error for ModelError {}

/// Something that answers one [`ModelRequest`] at a time, for each model under its name.
///
/// It is called and waited for. A model a plugin asks is a function: content goes in and an
/// answer comes back, and the model cannot act. Work that needs an agent is a session.
pub trait Model: Send + Sync {
    /// Answer `request`, waiting at most `timeout`. The request's `model` is this entry's
    /// own name for the model: what followed the first `/` of the name it was asked by.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::Failed`] if the model fails and [`ModelError::TimedOut`] if it
    /// takes longer than `timeout`.
    fn complete(&self, request: &ModelRequest, timeout: Duration)
    -> Result<ModelReply, ModelError>;

    /// What is known about `model`, by this entry's own name for it. Nothing, unless the
    /// implementation says.
    fn describe(&self, model: &str) -> ModelInfo {
        let _: &str = model;
        ModelInfo::unknown()
    }
}

impl<F> Model for F
where
    F: Fn(&ModelRequest, Duration) -> Result<ModelReply, ModelError> + Send + Sync,
{
    fn complete(
        &self,
        request: &ModelRequest,
        timeout: Duration,
    ) -> Result<ModelReply, ModelError> {
        self(request, timeout)
    }
}

/// `name` as the registry entry that serves it and that entry's own name for the model:
/// `<provider>/<model>`, divided at the first `/`. `None` if `name` has no `/` or names no
/// entry before it.
#[must_use]
pub fn split_name(name: &str) -> Option<(&str, &str)> {
    name.split_once('/')
        .filter(|(provider, _)| !provider.is_empty())
}

impl Registry<dyn Model> {
    /// Ask the model `request` names as `<provider>/<model>`. The entry registered as
    /// `<provider>` receives the request with `<model>` as its model.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::Unknown`] if the name has no provider or none is registered
    /// under it, or whatever [`Model::complete`] returns.
    pub fn complete(
        &self,
        mut request: ModelRequest,
        timeout: Duration,
    ) -> Result<ModelReply, ModelError> {
        let unknown = |name: &str| ModelError::Unknown(name.to_owned());
        let (provider, model) =
            split_name(&request.model).ok_or_else(|| unknown(&request.model))?;
        let entry = self.get(provider).ok_or_else(|| unknown(&request.model))?;
        request.model = model.to_owned();
        entry.complete(&request, timeout)
    }

    /// What is known about the model named `<provider>/<model>`, or `None` if nothing is
    /// registered as `<provider>`.
    #[must_use]
    pub fn describe(&self, name: &str) -> Option<ModelInfo> {
        let (provider, model) = split_name(name)?;
        Some(self.get(provider)?.describe(model))
    }
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::string::ToString;
    use alloc::sync::Arc;
    use alloc::vec;

    use super::*;

    /// Says which model it was asked as, and holds 1000 tokens if that is `small`.
    struct Echo;

    impl Model for Echo {
        fn complete(&self, request: &ModelRequest, _: Duration) -> Result<ModelReply, ModelError> {
            let said = format!("{} heard {}", request.model, request.messages.len());
            let usage = Usage::new(Some(3), Some(4)).with_reasoning(Some(1));
            Ok(ModelReply::new(Message::assistant().text(said), usage)
                .stopped(StopReason::Finished))
        }

        fn describe(&self, model: &str) -> ModelInfo {
            if model == "small" {
                ModelInfo::unknown().with_context(1000)
            } else {
                ModelInfo::unknown()
            }
        }
    }

    fn asking(model: &str) -> ModelRequest {
        ModelRequest::new(model, "", vec![Message::user().text("hi")])
    }

    #[test]
    fn a_model_is_asked_by_its_provider_and_its_own_name() {
        let models = Models::new();
        let echo: Arc<dyn Model> = Arc::new(Echo);
        models.insert("local", echo).unwrap();
        let wait = Duration::from_secs(1);

        // Divided at the first `/` only: the rest is the provider's business.
        let reply = models.complete(asking("local/org/small"), wait).unwrap();
        assert_eq!(reply.message.text_content(), "org/small heard 1");
        assert_eq!(reply.usage.reasoning_tokens, Some(1));
        assert_eq!(reply.stop, Some(StopReason::Finished));

        for name in ["other/small", "small", "/small"] {
            assert_eq!(
                models.complete(asking(name), wait),
                Err(ModelError::Unknown(name.into())),
                "{name}"
            );
        }
        assert_eq!(
            ModelError::TimedOut(wait).to_string(),
            "the model did not answer within 1000ms"
        );
    }

    #[test]
    fn a_closure_is_a_model_that_describes_nothing() {
        let models = Models::new();
        let echo: Arc<dyn Model> = Arc::new(|request: &ModelRequest, _: Duration| {
            Ok(ModelReply::new(
                Message::assistant().text(&request.system),
                Usage::default(),
            ))
        });
        models.insert("echo", echo).unwrap();
        let request = ModelRequest::new("echo/any", "be brief", Vec::new());

        let reply = models.complete(request, Duration::from_secs(1)).unwrap();
        assert_eq!(reply.message.text_content(), "be brief");
        assert_eq!(reply.stop, None);
        assert_eq!(models.describe("echo/any"), Some(ModelInfo::unknown()));
    }

    #[test]
    fn a_description_is_asked_by_name_and_corrected_field_by_field() {
        let models = Models::new();
        let echo: Arc<dyn Model> = Arc::new(Echo);
        models.insert("local", echo).unwrap();

        let reported = models.describe("local/small").unwrap();
        assert_eq!(reported.context_tokens, Some(1000));
        assert_eq!(models.describe("local/large"), Some(ModelInfo::unknown()));
        assert_eq!(models.describe("other/small"), None);
        assert_eq!(models.describe("small"), None);

        let corrected = ModelInfo::unknown()
            .with_context(8000)
            .with_tools(true)
            .taking([TEXT, AUDIO])
            .or(reported.with_output(500).taking([TEXT]).returning([TEXT]));
        assert_eq!(corrected.context_tokens, Some(8000));
        assert_eq!(corrected.output_tokens, Some(500));
        assert_eq!(corrected.tools, Some(true));
        // A list the host states replaces the provider's whole.
        assert_eq!(corrected.takes(AUDIO), Some(true));
        assert_eq!(corrected.takes(IMAGE), Some(false));
        assert_eq!(corrected.returns(TEXT), Some(true));
        // A kind the core has never heard of is a kind like any other.
        let embeds = ModelInfo::unknown().returning(["embedding"]);
        assert_eq!(embeds.returns("embedding"), Some(true));
        assert_eq!(ModelInfo::unknown().takes(VIDEO), None);
    }

    #[test]
    fn usage_names_its_counts_as_the_log_does() {
        let usage = Usage::new(Some(10), None).with_cache(Some(8), None);
        let given: Vec<_> = usage.counts().collect();
        assert_eq!(given, [("input_tokens", 10), ("cache_read_tokens", 8)]);
        assert_eq!(StopReason::Limit.to_string(), "limit");
        for stop in [
            StopReason::Finished,
            StopReason::Limit,
            StopReason::ToolCalls,
            StopReason::Refused,
        ] {
            assert_eq!(StopReason::parse(stop.as_str()), Some(stop));
        }
        assert_eq!(StopReason::parse("paused"), None);
    }
}
