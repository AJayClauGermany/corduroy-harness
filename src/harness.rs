//! Assembling the pieces into an agent.

use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use corduroy_core::{
    App, ContextError, Draft, EventStream, FiberId, KERNELS, Kernel, Kernels, PROMPTS, Plugin,
    Prompts, ShellKernel, Source, TOOLS, Tools, plugin_fn,
};
use oi_kit_agent::inference::InferenceRunner;
use oi_kit_agent::machine::{StateMachine, Step};
use oi_kit_agent::max_turns::MaxTurnsOperation;
use oi_kit_agent::operation::{Emitter, Operation, ends_turn};
use oi_kit_agent::tool::ToolOperation;
use oi_kit_agent::unhandled_tool_calls::UnhandledToolCallsOperation;
use oi_kit_provider_types::base::Provider;
use oi_kit_provider_types::conversation::message::{Message, MessageErrorKind};
use oi_kit_provider_types::model::ModelConfig;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::session::{Effect, Journal, Session};
use crate::tools::{PluginTools, PromptOperation};

/// The id of the one session a [`Harness`] runs.
const SESSION: &str = "main";

/// Why the harness could not be set up or a run could not complete.
#[derive(Debug)]
pub enum HarnessError {
    /// The plugin tree rejected a service or plugin.
    Context(ContextError),
    /// The agent loop failed in a way no operation recovered from.
    Run(String),
}

impl fmt::Display for HarnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Context(error) => write!(f, "plugin tree: {error}"),
            Self::Run(reason) => write!(f, "run failed: {reason}"),
        }
    }
}

impl Error for HarnessError {}

impl From<ContextError> for HarnessError {
    fn from(error: ContextError) -> Self {
        Self::Context(error)
    }
}

impl From<anyhow::Error> for HarnessError {
    fn from(error: anyhow::Error) -> Self {
        Self::Run(format!("{error:#}"))
    }
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// The model answered without asking for anything further.
    Finished {
        /// The model's final answer.
        answer: String,
    },
    /// The model was still working when it ran out of turns.
    TurnLimitReached,
    /// The model could not be reached or failed. The error is in the conversation.
    Failed {
        /// What went wrong.
        reason: String,
    },
    /// The loop stopped with nothing left to do but no answer, for example because an
    /// operation is waiting on the user.
    Yielded,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Finished { answer } => write!(f, "finished: {answer}"),
            Self::TurnLimitReached => f.write_str("stopped: turn limit reached"),
            Self::Failed { reason } => write!(f, "failed: {reason}"),
            Self::Yielded => f.write_str("yielded"),
        }
    }
}

/// The result of a completed run.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// How the run ended.
    pub status: Status,
    /// The session, including the whole conversation.
    pub session: Session,
}

/// An agent: a plugin tree, an event stream and a programmable loop around a model.
///
/// The harness itself does very little. It provides the three registries plugins contribute
/// to ([`Tools`], [`Prompts`], [`Kernels`]), journals the conversation to the event stream,
/// and lines up oi-kit operations into a loop. Tools, prompt text, kernels and reactions to
/// events all come from plugins.
pub struct Harness {
    app: App,
    tools: Arc<Tools>,
    prompts: Arc<Prompts>,
    kernels: Arc<Kernels>,
    journal: Arc<Journal>,
    provider: Arc<dyn Provider>,
    model: ModelConfig,
    system: String,
    max_turns: u32,
    operations: Vec<Arc<dyn Operation<Session, Effect>>>,
}

impl fmt::Debug for Harness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Harness")
            .field("app", &self.app)
            .field("tools", &self.tools)
            .field("model", &self.model.model_name)
            .field("max_turns", &self.max_turns)
            .finish_non_exhaustive()
    }
}

impl Harness {
    /// A harness around `provider` with no plugins and an empty event stream.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::Context`] if the built-in services cannot be provided.
    pub fn new(provider: Arc<dyn Provider>) -> Result<Self, HarnessError> {
        Self::resume(provider, Arc::new(EventStream::new()))
    }

    /// A harness that continues from an existing event stream, such as one rebuilt with
    /// [`EventStream::replay`]. The conversation is whatever the stream's `message` events
    /// add up to.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::Run`] if the stream's messages cannot be decoded, or
    /// [`HarnessError::Context`] if the built-in services cannot be provided.
    pub fn resume(
        provider: Arc<dyn Provider>,
        events: Arc<EventStream>,
    ) -> Result<Self, HarnessError> {
        let journal = Arc::new(Journal::new(SESSION, Arc::clone(&events))?);
        let tools = Arc::new(Tools::new());
        let prompts = Arc::new(Prompts::new());
        let kernels = Arc::new(Kernels::new());
        let mut app = App::with_events(events);
        app.provide(TOOLS, Arc::clone(&tools))?;
        app.provide(PROMPTS, Arc::clone(&prompts))?;
        app.provide(KERNELS, Arc::clone(&kernels))?;
        Ok(Self {
            app,
            tools,
            prompts,
            kernels,
            journal,
            provider,
            model: ModelConfig::new("default"),
            system: String::new(),
            max_turns: 25,
            operations: Vec::new(),
        })
    }

    /// Ask the provider for `model`.
    #[must_use]
    pub fn with_model(mut self, model: &str) -> Self {
        self.model = ModelConfig::new(model);
        self
    }

    /// Start every system prompt with `text`. Plugins' prompt parts follow it.
    #[must_use]
    pub fn with_system_prompt(mut self, text: impl Into<String>) -> Self {
        self.system = text.into();
        self
    }

    /// Stop a run after this many model turns without an answer.
    #[must_use]
    pub const fn with_max_turns(mut self, max_turns: u32) -> Self {
        self.max_turns = max_turns;
        self
    }

    /// Add a step to the loop, ahead of the built-in ones. Steps are tried in order and the
    /// first that applies wins, so an operation added here can act before tools run or the
    /// model is called: approve calls, compact history, answer a command.
    #[must_use]
    pub fn with_operation(mut self, operation: Arc<dyn Operation<Session, Effect>>) -> Self {
        self.operations.push(operation);
        self
    }

    /// Load a plugin.
    pub fn plugin(&mut self, plugin: impl Plugin + 'static) -> FiberId {
        self.app.plugin(plugin)
    }

    /// The plugin tree, for unloading and reloading plugins.
    pub const fn app(&mut self) -> &mut App {
        &mut self.app
    }

    /// The stream every event of this harness is recorded on.
    #[must_use]
    pub const fn events(&self) -> &Arc<EventStream> {
        self.app.events()
    }

    /// The tools plugins have registered.
    #[must_use]
    pub const fn tools(&self) -> &Arc<Tools> {
        &self.tools
    }

    /// The kernels plugins have registered.
    #[must_use]
    pub const fn kernels(&self) -> &Arc<Kernels> {
        &self.kernels
    }

    /// The journal that persists effects to the event stream. It is the `runtime` argument
    /// for callers that drive a [`StateMachine`] themselves.
    #[must_use]
    pub const fn journal(&self) -> &Arc<Journal> {
        &self.journal
    }

    /// The loop, as the ordered steps a [`StateMachine`] walks: custom operations, the
    /// prompt, plugin tools, an answer for calls nobody handled, the turn limit and finally
    /// the model.
    #[must_use]
    pub fn steps(&self) -> Vec<Step<'static, Session, Effect>> {
        let tools =
            ToolOperation::new().with_provider(Arc::new(PluginTools::new(Arc::clone(&self.tools))));
        let prompt = PromptOperation::new(self.system.clone(), Arc::clone(&self.prompts));
        let model: InferenceRunner<'static, Session, Effect> =
            InferenceRunner::new(Arc::clone(&self.provider), self.model.clone());
        let mut steps: Vec<Step<'static, Session, Effect>> = self
            .operations
            .iter()
            .map(|operation| Step::Operation(Arc::clone(operation)))
            .collect();
        steps.push(Step::Operation(Arc::new(prompt)));
        steps.push(Step::Operation(Arc::new(tools)));
        steps.push(Step::Operation(
            Arc::new(UnhandledToolCallsOperation::new()),
        ));
        steps.push(Step::Operation(Arc::new(MaxTurnsOperation::new(
            self.max_turns,
        ))));
        steps.push(Step::Inference(Arc::new(model)));
        steps
    }

    /// A state machine over [`steps`](Self::steps). Call its `step` and `apply` with
    /// [`journal`](Self::journal) to advance the loop one step at a time.
    #[must_use]
    pub fn machine(&self, cancel: CancellationToken) -> StateMachine<'static, Session, Effect> {
        StateMachine::new(self.steps(), cancel)
    }

    /// Add `prompt` to the conversation as the user and run the loop until the model
    /// answers, runs out of turns or fails.
    ///
    /// Tool failures and model failures are recorded in the conversation, not returned from
    /// here: see [`Status`].
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::Run`] if an operation fails outright, for example because two
    /// plugins registered tools the model cannot tell apart.
    pub async fn run(&self, prompt: &str) -> Result<Outcome, HarnessError> {
        let prompt = Message::user()
            .with_text(prompt)
            .with_generated_id_if_missing();
        self.journal.record(&prompt)?;
        let cancel = CancellationToken::new();
        let machine = self.machine(cancel.clone());
        // Streaming updates have no audience here; the event stream is the record.
        let (updates, _unwatched) = mpsc::unbounded_channel();
        let emitter = Emitter::new(updates, cancel);
        let session = machine
            .run(self.journal.as_ref(), SESSION, &emitter)
            .await?;
        self.events().emit(Draft::new(Source::Host, "turn/end"));
        Ok(Outcome {
            status: status_of(&session),
            session,
        })
    }
}

fn status_of(session: &Session) -> Status {
    let messages = session.conversation().messages();
    let Some(last) = messages.last() else {
        return Status::Yielded;
    };
    match last.error_kind() {
        Some(MessageErrorKind::ResourceExhausted) => Status::TurnLimitReached,
        Some(_) => Status::Failed {
            reason: last
                .error()
                .map_or_else(String::new, |error| error.message.clone()),
        },
        // An assistant message that asks for nothing further is the answer.
        None if ends_turn(messages) => Status::Finished {
            answer: last.as_concat_text(),
        },
        None => Status::Yielded,
    }
}

/// A plugin that provides a persistent shell as the kernel `name`, working in `dir`.
///
/// Unloading the plugin stops the shell and removes the kernel.
pub fn shell_kernel(name: impl Into<String>, dir: impl Into<PathBuf>) -> impl Plugin + 'static {
    let name = name.into();
    let dir = dir.into();
    plugin_fn(format!("kernel-{name}"), &[KERNELS], move |ctx| {
        let kernels = ctx.service::<Kernels>(KERNELS)?;
        let kernel: Arc<Mutex<dyn Kernel>> =
            Arc::new(Mutex::new(ShellKernel::new().in_dir(dir.clone())));
        kernels.register(ctx, name.clone(), kernel)?;
        Ok(())
    })
}
