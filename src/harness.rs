//! Assembling the pieces into an agent.

use std::error::Error;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use corduroy_core::{
    App, ContextError, EventStream, FiberId, GATES, Gates, HTTP, HttpClients, KERNELS, Kernels,
    LogError, MODELS, Models, PROMPTS, Plugin, Prompts, Registry, SECRETS, SERVICES, Secrets,
    Services, SessionId, TOOLS, Tools,
};
use oi_kit_agent::inference::InferenceRunner;
use oi_kit_agent::machine::{StateMachine, Step};
use oi_kit_agent::max_turns::MaxTurnsOperation;
use oi_kit_agent::operation::{Emitter, Operation, ends_turn, messages_since_kickoff};
use oi_kit_agent::tool::ToolOperation;
use oi_kit_agent::tool_call_state::pending_tool_requests;
use oi_kit_agent::unhandled_tool_calls::UnhandledToolCallsOperation;
use oi_kit_provider_types::base::Provider;
use oi_kit_provider_types::conversation::message::{Message, MessageErrorKind};
use oi_kit_provider_types::model::ModelConfig;
use rmcp::model::{CallToolResult, ContentBlock};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::children::{SESSIONS, SessionsService};
use crate::compact::CompactOperation;
use crate::gates::{GateOperation, SettleOperation, WaitOperation};
use crate::provider::{DEFAULT_PROVIDER, PROVIDERS, Providers, Routed};
use crate::session::{
    Answer, Ask, Effect, Journal, MAIN, Progress, Session, SessionInfo, Sessions, Wait, blocks_of,
    ended,
};
use crate::tools::{PluginTools, PromptOperation};

/// Name of the service under which the harness provides the [`Operations`] registry.
pub const OPERATIONS: &str = "operations";

/// Steps of the loop that native plugins contribute.
///
/// A step is an oi-kit [`Operation`]: it looks at the session at the start of a step of the
/// loop and either declines or returns [`Effect`]s for the journal to store. Registered
/// steps are tried in name order, after those the host added with
/// [`Harness::with_operation`] and ahead of the built-in ones, so one can act before tools
/// run or the model is asked. A step registered through a plugin's context leaves with the
/// plugin.
///
/// The steps are read whenever a run is started or taken on, so a step registered while a
/// run is going joins the loop from the next time it stops (to wait, or at its end).
///
/// Operations are native code and run with the host's authority; a WebAssembly plugin
/// shapes the loop through gates, tools and events instead.
pub type Operations = Registry<dyn Operation<Session, Effect>>;

/// Why the harness could not be set up or a run could not complete.
#[derive(Debug)]
pub enum HarnessError {
    /// The plugin tree rejected a service or plugin.
    Context(ContextError),
    /// The agent loop failed in a way no operation recovered from.
    Run(String),
    /// The event log could not store a commit. Nothing after it was recorded or shown.
    Log(LogError),
    /// No session has this id.
    UnknownSession(SessionId),
    /// A session with this id already exists.
    DuplicateSession(SessionId),
    /// The session has a run that never reached its end, so it cannot take a new prompt.
    /// [`Harness::continue_run`] finishes it.
    Unfinished(SessionId),
    /// The session's run is waiting for answers to gates that asked about its tool calls, so
    /// it cannot take a new prompt. [`Harness::answer`] gives them.
    Waiting(SessionId),
    /// No gate of this session is waiting for an answer about this tool call.
    NotAsked {
        /// The session.
        session: SessionId,
        /// The tool call that was answered.
        call: String,
    },
}

impl fmt::Display for HarnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Context(error) => write!(f, "plugin tree: {error}"),
            Self::Run(reason) => write!(f, "run failed: {reason}"),
            Self::Log(error) => write!(f, "{error}"),
            Self::UnknownSession(id) => write!(f, "there is no session `{id}`"),
            Self::DuplicateSession(id) => write!(f, "session `{id}` already exists"),
            Self::Unfinished(id) => {
                write!(f, "session `{id}` has an unfinished run: continue it first")
            }
            Self::Waiting(id) => {
                write!(
                    f,
                    "session `{id}` is waiting for an answer: answer it first"
                )
            }
            Self::NotAsked { session, call } => write!(
                f,
                "nothing in session `{session}` is waiting for an answer about `{call}`"
            ),
        }
    }
}

impl Error for HarnessError {}

impl From<ContextError> for HarnessError {
    fn from(error: ContextError) -> Self {
        Self::Context(error)
    }
}

impl From<LogError> for HarnessError {
    fn from(error: LogError) -> Self {
        Self::Log(error)
    }
}

impl From<anyhow::Error> for HarnessError {
    fn from(error: anyhow::Error) -> Self {
        // A refused commit keeps its type on the way up through the loop.
        match error.downcast::<LogError>() {
            Ok(error) => Self::Log(error),
            Err(error) => Self::Run(format!("{error:#}")),
        }
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
    /// The run waits, in the log, and survives a restart. Gates asked about tool calls,
    /// here or in a child session this run waits for, and a person's answer to each goes
    /// through [`Harness::answer`]; or a tool call waits for a signal that
    /// [`Harness::signal`] brings.
    Waiting {
        /// The calls waiting for a person's answer.
        asks: Vec<Ask>,
        /// The calls waiting for a signal.
        waits: Vec<Wait>,
    },
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Finished { answer } => write!(f, "finished: {answer}"),
            Self::TurnLimitReached => f.write_str("stopped: turn limit reached"),
            Self::Failed { reason } => write!(f, "failed: {reason}"),
            Self::Yielded => f.write_str("yielded"),
            Self::Waiting { asks, waits } if asks.is_empty() => {
                let signals: Vec<&str> = waits.iter().map(|wait| wait.signal.as_str()).collect();
                write!(f, "waiting for {}", signals.join(", "))
            }
            Self::Waiting { asks, .. } => write!(f, "waiting for {} answer(s)", asks.len()),
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
/// A harness holds any number of sessions, each a conversation of its own on the one stream.
/// It starts with the session [`MAIN`], which [`run`](Self::run) uses.
///
/// The harness itself does very little. It provides the registries plugins contribute to
/// ([`Tools`], [`Prompts`], [`Kernels`], [`Gates`], [`Services`], [`Providers`], [`Models`],
/// [`Secrets`], [`HttpClients`]), journals the conversation to the event stream, and lines up
/// oi-kit operations into a loop. The model, tools, prompt text, kernels and reactions to
/// events all come from plugins.
pub struct Harness {
    /// Shared with the step that applies changes plugins asked for while the loop runs.
    app: Arc<Mutex<App>>,
    events: Arc<EventStream>,
    tools: Arc<Tools>,
    prompts: Arc<Prompts>,
    kernels: Arc<Kernels>,
    gates: Arc<Gates>,
    services: Arc<Services>,
    providers: Arc<Providers>,
    models: Arc<Models>,
    secrets: Arc<Secrets>,
    http: Arc<HttpClients>,
    sessions: Arc<Sessions>,
    /// The name of the provider the loop asks, looked up in `providers` on every request.
    provider: String,
    model: ModelConfig,
    system: String,
    max_turns: u32,
    restart_limit: u32,
    /// Steps the host added, tried first and in the order given.
    operations: Vec<Arc<dyn Operation<Session, Effect>>>,
    /// Steps plugins registered, tried next and in name order.
    contributed: Arc<Operations>,
}

impl fmt::Debug for Harness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Harness")
            .field("app", &self.app)
            .field("tools", &self.tools)
            .field("provider", &self.provider)
            .field("model", &self.model.model_name)
            .field("max_turns", &self.max_turns)
            .finish_non_exhaustive()
    }
}

impl Harness {
    /// A harness around `provider` with an empty event stream. See [`resume`](Self::resume).
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::Context`] if the built-in services cannot be provided.
    pub fn new(provider: Arc<dyn Provider>) -> Result<Self, HarnessError> {
        Self::resume(provider, Arc::new(EventStream::new()))
    }

    /// A harness that continues from an existing event stream, such as one rebuilt with
    /// [`EventStream::replay`] or opened from a log on disk. Its sessions are the ones the
    /// stream holds, and each conversation is whatever that session's `message` events add
    /// up to.
    ///
    /// `provider` is registered in [`Providers`] as [`DEFAULT_PROVIDER`], which the loop asks
    /// unless [`with_provider`](Self::with_provider) names another. It belongs to the host
    /// rather than to a plugin; to have plugins supply the model, start with
    /// [`open`](Self::open) and load a [`provider`](crate::provider()) plugin.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::Log`] if the stream cannot store the start of the main
    /// session, or [`HarnessError::Context`] if the built-in services cannot be provided.
    pub fn resume(
        provider: Arc<dyn Provider>,
        events: Arc<EventStream>,
    ) -> Result<Self, HarnessError> {
        let harness = Self::open(events)?;
        harness
            .providers
            .insert(DEFAULT_PROVIDER, provider)
            .map_err(|duplicate| HarnessError::Run(duplicate.to_string()))?;
        Ok(harness)
    }

    /// A harness over `events` with no plugins at all, not even a provider: the model comes
    /// from a plugin registered in [`Providers`], such as [`provider`](crate::provider()).
    /// Until one is there, every model request fails and the run ends as [`Status::Failed`].
    ///
    /// # Errors
    ///
    /// As [`resume`](Self::resume).
    pub fn open(events: Arc<EventStream>) -> Result<Self, HarnessError> {
        let sessions = Arc::new(Sessions::load(Arc::clone(&events))?);
        let tools = Arc::new(Tools::new());
        let prompts = Arc::new(Prompts::new());
        let kernels = Arc::new(Kernels::new());
        let gates = Arc::new(Gates::new());
        let services = Arc::new(Services::new());
        let providers = Arc::new(Providers::new());
        let models = Arc::new(Models::new());
        let secrets = Arc::new(Secrets::new());
        let http = Arc::new(HttpClients::new());
        let contributed = Arc::new(Operations::new());
        let mut app = App::with_events(Arc::clone(&events));
        app.provide(TOOLS, Arc::clone(&tools))?;
        app.provide(PROMPTS, Arc::clone(&prompts))?;
        app.provide(KERNELS, Arc::clone(&kernels))?;
        app.provide(GATES, Arc::clone(&gates))?;
        app.provide(SERVICES, Arc::clone(&services))?;
        app.provide(PROVIDERS, Arc::clone(&providers))?;
        app.provide(MODELS, Arc::clone(&models))?;
        app.provide(SECRETS, Arc::clone(&secrets))?;
        app.provide(HTTP, Arc::clone(&http))?;
        app.provide(OPERATIONS, Arc::clone(&contributed))?;
        let children: Arc<dyn corduroy_core::Service> = Arc::new(SessionsService {
            sessions: Arc::clone(&sessions),
        });
        services
            .insert(SESSIONS, children)
            .map_err(|duplicate| HarnessError::Run(duplicate.to_string()))?;
        Ok(Self {
            app: Arc::new(Mutex::new(app)),
            events,
            tools,
            prompts,
            kernels,
            gates,
            services,
            providers,
            models,
            secrets,
            http,
            sessions,
            provider: DEFAULT_PROVIDER.to_owned(),
            model: ModelConfig::new("default"),
            system: String::new(),
            max_turns: 25,
            restart_limit: 5,
            operations: Vec::new(),
            contributed,
        })
    }

    /// Ask the provider registered as `name` in [`Providers`], rather than
    /// [`DEFAULT_PROVIDER`].
    #[must_use]
    pub fn with_provider(mut self, name: impl Into<String>) -> Self {
        self.provider = name.into();
        self
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

    /// Give up on an unfinished run once it has been picked up this many times without
    /// reaching its end, so a step that crashes the process every time does not do so
    /// forever. See [`continue_run`](Self::continue_run).
    #[must_use]
    pub const fn with_restart_limit(mut self, restart_limit: u32) -> Self {
        self.restart_limit = restart_limit;
        self
    }

    /// Add a step to the loop, ahead of the built-in ones. Steps are tried in order and the
    /// first that applies wins, so an operation added here can act before tools run or the
    /// model is called: approve calls, compact history, answer a command.
    ///
    /// This is for the host. A native plugin registers its steps in [`Operations`] instead
    /// ([`operations`](Self::operations)), so that they leave with it.
    #[must_use]
    pub fn with_operation(mut self, operation: Arc<dyn Operation<Session, Effect>>) -> Self {
        self.operations.push(operation);
        self
    }

    /// Load a plugin.
    pub fn plugin(&self, plugin: impl Plugin + 'static) -> FiberId {
        self.app().plugin(plugin)
    }

    /// The plugin tree, for unloading and reloading plugins. Changes plugins ask for through
    /// [`Requests`](corduroy_core::Requests) are applied before each step of the loop, and by
    /// any change made here.
    pub fn app(&self) -> MutexGuard<'_, App> {
        self.app.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The stream every event of this harness is recorded on.
    #[must_use]
    pub const fn events(&self) -> &Arc<EventStream> {
        &self.events
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

    /// The gates plugins have registered.
    #[must_use]
    pub const fn gates(&self) -> &Arc<Gates> {
        &self.gates
    }

    /// The services plugins provide to each other.
    #[must_use]
    pub const fn services(&self) -> &Arc<Services> {
        &self.services
    }

    /// The model providers plugins have registered. The loop asks one of them.
    #[must_use]
    pub const fn providers(&self) -> &Arc<Providers> {
        &self.providers
    }

    /// The models plugins may ask for a completion of their own.
    #[must_use]
    pub const fn models(&self) -> &Arc<Models> {
        &self.models
    }

    /// The secrets plugins may use without seeing.
    #[must_use]
    pub const fn secrets(&self) -> &Arc<Secrets> {
        &self.secrets
    }

    /// The HTTP clients the `http` capability sends through, by URL scheme.
    #[must_use]
    pub const fn http(&self) -> &Arc<HttpClients> {
        &self.http
    }

    /// The steps of the loop plugins have registered.
    #[must_use]
    pub const fn operations(&self) -> &Arc<Operations> {
        &self.contributed
    }

    /// Start a session called `id`, recording `parent` as the session it came from. The new
    /// session has an empty conversation of its own.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::DuplicateSession`] if `id` is taken,
    /// [`HarnessError::UnknownSession`] if `parent` does not exist, or [`HarnessError::Log`]
    /// if the start could not be stored.
    pub fn start_session(
        &self,
        id: impl Into<SessionId>,
        parent: Option<&SessionId>,
    ) -> Result<SessionId, HarnessError> {
        self.sessions.start(id.into(), parent)
    }

    /// Every session, in order of id.
    #[must_use]
    pub fn session_list(&self) -> Vec<SessionInfo> {
        self.sessions.list()
    }

    /// The journal that persists the effects of `session` to the event stream. It is the
    /// `runtime` argument for callers that drive a [`StateMachine`] themselves.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::UnknownSession`] if there is no such session, or
    /// [`HarnessError::Run`] if its events cannot be decoded.
    pub fn journal(&self, session: &SessionId) -> Result<Arc<Journal>, HarnessError> {
        self.sessions.journal(session)
    }

    /// The loop, as the ordered steps a [`StateMachine`] walks: applying the plugin changes
    /// plugins asked for, the host's operations, the operations plugins registered (in name
    /// order), compaction, the prompt, the gates, plugin tools, an answer for calls nobody
    /// handled, a stop while a call waits for a signal, the turn limit and finally the model.
    #[must_use]
    pub fn steps(&self) -> Vec<Step<'static, Session, Effect>> {
        let tools = ToolOperation::new().with_provider(Arc::new(PluginTools::new(
            Arc::clone(&self.tools),
            Arc::clone(&self.sessions),
        )));
        let prompt = PromptOperation::new(self.system.clone(), Arc::clone(&self.prompts));
        let routed = Routed {
            providers: Arc::clone(&self.providers),
            name: self.provider.clone(),
        };
        let model: InferenceRunner<'static, Session, Effect> =
            InferenceRunner::new(Arc::new(routed), self.model.clone());
        let mut steps: Vec<Step<'static, Session, Effect>> = self
            .operations
            .iter()
            .map(|operation| Step::Operation(Arc::clone(operation)))
            .collect();
        steps.insert(
            0,
            Step::Operation(Arc::new(SettleOperation(Arc::clone(&self.app)))),
        );
        steps.extend(self.contributed.entries().into_iter().map(Step::Operation));
        steps.push(Step::Operation(Arc::new(CompactOperation::new(
            Arc::clone(&self.services),
            Arc::clone(&self.sessions),
        ))));
        steps.push(Step::Operation(Arc::new(prompt)));
        steps.push(Step::Operation(Arc::new(GateOperation::new(
            Arc::clone(&self.gates),
            Arc::clone(&self.tools),
            Arc::clone(&self.sessions),
        ))));
        steps.push(Step::Operation(Arc::new(tools)));
        steps.push(Step::Operation(
            Arc::new(UnhandledToolCallsOperation::new()),
        ));
        steps.push(Step::Operation(Arc::new(WaitOperation::new(Arc::clone(
            &self.sessions,
        )))));
        steps.push(Step::Operation(Arc::new(MaxTurnsOperation::new(
            self.max_turns,
        ))));
        steps.push(Step::Inference(Arc::new(model)));
        steps
    }

    /// A state machine over [`steps`](Self::steps). Call its `step` and `apply` with a
    /// session's [`journal`](Self::journal) to advance the loop one step at a time.
    #[must_use]
    pub fn machine(&self, cancel: CancellationToken) -> StateMachine<'static, Session, Effect> {
        StateMachine::new(self.steps(), cancel)
    }

    /// Add `prompt` to the main session's conversation as the user and run the loop until
    /// the model answers, runs out of turns or fails. See [`run_in`](Self::run_in).
    ///
    /// # Errors
    ///
    /// As [`run_in`](Self::run_in).
    pub async fn run(&self, prompt: &str) -> Result<Outcome, HarnessError> {
        self.run_in(&SessionId::new(MAIN), prompt).await
    }

    /// Add `prompt` to the conversation of `session` as the user and run the loop until the
    /// model answers, runs out of turns or fails.
    ///
    /// Tool failures and model failures are recorded in the conversation, not returned from
    /// here: see [`Status`].
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::UnknownSession`] if there is no such session,
    /// [`HarnessError::Waiting`] if its last run waits for an answer,
    /// [`HarnessError::Unfinished`] if its last run never reached its end otherwise,
    /// [`HarnessError::Run`] if an operation fails outright, for example because two plugins
    /// registered tools the model cannot tell apart, or [`HarnessError::Log`] if the event
    /// log could not store a step.
    pub async fn run_in(&self, session: &SessionId, prompt: &str) -> Result<Outcome, HarnessError> {
        let journal = self.journal(session)?;
        if !journal.waiting().is_empty() || !journal.awaiting().is_empty() {
            return Err(HarnessError::Waiting(session.clone()));
        }
        if journal.unfinished() {
            return Err(HarnessError::Unfinished(session.clone()));
        }
        let prompt = Message::user()
            .with_text(prompt)
            .with_generated_id_if_missing();
        journal.record(&prompt)?;
        self.drive(&journal).await
    }

    /// Pick up the run of `session` that never reached its end, because the process died or
    /// [`run_in`](Self::run_in) returned an error, and take it to its end without a new
    /// prompt. If the session has no such run, nothing happens and the outcome is that of
    /// its last one.
    ///
    /// The log says how far each tool call the model is still waiting on got:
    ///
    /// - It returned, and only handing the result to the model was cut short: the result is
    ///   taken from the log and the tool is not run again.
    /// - It never started: it runs now, like any other call.
    /// - It was started and nothing more is known: it is made again if its tool is
    ///   [`replay_safe`](field@corduroy_core::ToolSpec::replay_safe). Otherwise it is answered
    ///   with an error saying it was interrupted, and the model decides what to do, as with
    ///   any other tool failure.
    ///
    /// Each call counts as a restart of the run. Past the
    /// [restart limit](Self::with_restart_limit) the run ends as [`Status::Failed`].
    ///
    /// A run that is waiting for an answer is not picked up, nor counted: the outcome is
    /// [`Status::Waiting`] until [`answer`](Self::answer) is called. A run waiting for a
    /// child session takes the child on from where it stopped, and then itself.
    ///
    /// # Errors
    ///
    /// As [`run_in`](Self::run_in), except that an unfinished run is what this is for.
    pub async fn continue_run(&self, session: &SessionId) -> Result<Outcome, HarnessError> {
        let journal = self.journal(session)?;
        if !journal.awaiting().is_empty() && journal.waiting().is_empty() {
            // A crash after a call began waiting but before its placeholder reached the
            // conversation leaves the call unanswered: answer it from the log, or the tools
            // step would make it again (and start a second child session).
            if let Some(settled) = self.settled(&journal) {
                journal.record(&settled)?;
            }
            return self.drive(&journal).await;
        }
        if !journal.unfinished() || !journal.waiting().is_empty() {
            return Ok(self.outcome_of(&journal));
        }
        let attempt = journal.restart()?;
        if attempt > self.restart_limit {
            let reason = format!(
                "the run was restarted {} times without finishing and was given up",
                self.restart_limit
            );
            let failure = Message::assistant()
                .with_error(MessageErrorKind::Other, reason)
                .with_generated_id_if_missing();
            journal.record(&failure)?;
            journal.end_turn()?;
            return Ok(self.outcome_of(&journal));
        }
        if let Some(settled) = self.settled(&journal) {
            journal.record(&settled)?;
        }
        self.drive(&journal).await
    }

    /// Results for the unanswered tool calls of an interrupted run that must not simply be
    /// run: those the log already has a result for, and those that were started and are
    /// not safe to make again. Calls left out are for the tools step to run.
    fn settled(&self, journal: &Journal) -> Option<Message> {
        let session = journal.session();
        let turn = messages_since_kickoff(session.conversation()).ok()?;
        let mut results = Message::user();
        for request in pending_tool_requests(turn) {
            // A call that could not be decoded never ran; the tools step answers it.
            let Ok(call) = request.tool_call.as_ref() else {
                continue;
            };
            let result = match journal.progress(&request.id) {
                Progress::NotStarted => continue,
                Progress::Started if self.replay_safe(&call.name) => continue,
                Progress::Started => CallToolResult::error(vec![ContentBlock::text(format!(
                    "interrupted: the harness stopped while `{}` was running, and it is not \
                     safe to run twice. Check what it did before calling it again.",
                    call.name
                ))]),
                // The result as the tool returned it, images included, from the log.
                Progress::Finished { ok: true, output } => {
                    CallToolResult::success(blocks_of(&output))
                }
                Progress::Finished { ok: false, output } => {
                    CallToolResult::error(blocks_of(&output))
                }
            };
            results = results.with_tool_response(request.id.clone(), Ok(result));
        }
        (!results.content.is_empty()).then(|| results.with_generated_id_if_missing())
    }

    /// Answer the gate that asked about the tool call `call` in `session`. Once every call
    /// the run waits for is answered, the run goes on: allowed calls run, declined ones go
    /// back to the model as errors carrying the reason, and the loop continues until the run
    /// ends or waits again. If `session` is a child session another run waits for, that run
    /// goes on too once the child ends, and the outcome is the outermost one's.
    ///
    /// The answer is stored before anything else happens, so a crash after it loses
    /// nothing: [`continue_run`](Self::continue_run) takes the run on from there.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::NotAsked`] if no gate of the session waits for an answer about
    /// `call`, and otherwise as [`run_in`](Self::run_in).
    pub async fn answer(
        &self,
        session: &SessionId,
        call: &str,
        answer: Answer,
    ) -> Result<Outcome, HarnessError> {
        let journal = self.journal(session)?;
        if !journal.answer(call, &answer)? {
            return Err(HarnessError::NotAsked {
                session: session.clone(),
                call: call.to_owned(),
            });
        }
        if !journal.waiting().is_empty() {
            return Ok(self.outcome_of(&journal));
        }
        let outcome = self.drive(&journal).await?;
        self.resume_parents(session, outcome).await
    }

    /// Bring `payload` to the calls of `session` waiting for the signal `name`, and take the
    /// run on. Signals nobody in the session waits for are not stored.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::Run`] if nothing in `session` waits for `name`, and otherwise
    /// as [`run_in`](Self::run_in).
    pub async fn signal(
        &self,
        session: &SessionId,
        name: &str,
        payload: &str,
    ) -> Result<Outcome, HarnessError> {
        let journal = self.journal(session)?;
        if !journal.signal(name, payload)? {
            return Err(HarnessError::Run(format!(
                "nothing in session `{session}` waits for the signal `{name}`"
            )));
        }
        let outcome = self.drive(&journal).await?;
        self.resume_parents(session, outcome).await
    }

    /// Once `session` has ended, take on each session up the chain that waits for it.
    async fn resume_parents(
        &self,
        session: &SessionId,
        mut outcome: Outcome,
    ) -> Result<Outcome, HarnessError> {
        let mut current = session.clone();
        loop {
            let parent = self
                .session_list()
                .into_iter()
                .find(|info| info.id == current)
                .and_then(|info| info.parent);
            let Some(parent) = parent else {
                return Ok(outcome);
            };
            let journal = self.journal(&parent)?;
            let waits_for_us = journal
                .awaiting()
                .iter()
                .any(|wait| ended(&wait.signal).as_ref() == Some(&current));
            if !waits_for_us {
                return Ok(outcome);
            }
            outcome = Box::pin(self.drive(&journal)).await?;
            current = parent;
        }
    }

    fn replay_safe(&self, tool: &str) -> bool {
        self.tools
            .get(tool)
            .is_some_and(|tool| tool.spec().replay_safe)
    }

    /// Run the loop over `journal`'s session until it stops, run the child sessions it then
    /// waits for, and go on once they end. Marks the end of the turn unless the run waits.
    async fn drive(&self, journal: &Journal) -> Result<Outcome, HarnessError> {
        loop {
            self.step_until_stopped(journal).await?;
            let awaiting = journal.awaiting();
            if awaiting.is_empty() && journal.waiting().is_empty() {
                journal.end_turn()?;
            }
            if awaiting.is_empty() || !self.run_children(journal, &awaiting).await? {
                return Ok(self.outcome_of(journal));
            }
        }
    }

    /// Run the loop over `journal`'s session until no step applies or one yields.
    async fn step_until_stopped(&self, journal: &Journal) -> Result<(), HarnessError> {
        let cancel = CancellationToken::new();
        let machine = self.machine(cancel.clone());
        // Streaming updates have no audience here; the event stream is the record.
        let (updates, _unwatched) = mpsc::unbounded_channel();
        let emitter = Emitter::new(updates, cancel);
        machine
            .run(journal, journal.id().as_str(), &emitter)
            .await?;
        Ok(())
    }

    /// Run each child session `awaiting` waits for, and bring the end of those that ended
    /// to `journal`. Returns whether any did.
    async fn run_children(
        &self,
        journal: &Journal,
        awaiting: &[Wait],
    ) -> Result<bool, HarnessError> {
        let mut resolved = false;
        for wait in awaiting {
            let Some(child) = ended(&wait.signal) else {
                continue;
            };
            if let Some(payload) = Box::pin(self.run_child(&child)).await? {
                resolved |= journal.signal(&wait.signal, &payload)?;
            }
        }
        Ok(resolved)
    }

    /// Take the child session `child` as far as it goes. Returns what its end brings to the
    /// call waiting for it, or `None` if it waits itself.
    async fn run_child(&self, child: &SessionId) -> Result<Option<String>, HarnessError> {
        let journal = self.journal(child)?;
        let started = !journal.session().conversation().is_empty();
        let outcome = if !started {
            let task = self
                .session_list()
                .into_iter()
                .find(|info| info.id == *child)
                .and_then(|info| info.task)
                .unwrap_or_default();
            let prompt = Message::user()
                .with_text(&task)
                .with_generated_id_if_missing();
            journal.record(&prompt)?;
            self.drive(&journal).await?
        } else if journal.unfinished() || !journal.awaiting().is_empty() {
            self.continue_run(child).await?
        } else {
            self.outcome_of(&journal)
        };
        Ok(brought(&outcome))
    }

    /// How `journal`'s session stands, with the asks of the child sessions it waits for.
    fn outcome_of(&self, journal: &Journal) -> Outcome {
        let session = journal.session();
        let mut asks = journal.waiting();
        let waits = journal.awaiting();
        let children = waits
            .iter()
            .filter_map(|wait| ended(&wait.signal))
            .filter_map(|child| self.journal(&child).ok());
        for child in children {
            if let Status::Waiting { asks: theirs, .. } = self.outcome_of(&child).status {
                asks.extend(theirs);
            }
        }
        let status = if asks.is_empty() && waits.is_empty() {
            status_of(&session)
        } else {
            Status::Waiting { asks, waits }
        };
        Outcome { status, session }
    }
}

/// What a child session's end brings to the call that waits for it: its answer, or why it
/// has none. `None` while it is still waiting.
fn brought(outcome: &Outcome) -> Option<String> {
    let last = || {
        outcome
            .session
            .conversation()
            .messages()
            .iter()
            .rev()
            .map(Message::as_concat_text)
            .find(|text| !text.trim().is_empty())
            .unwrap_or_default()
    };
    match &outcome.status {
        Status::Finished { answer } => Some(answer.clone()),
        Status::Failed { reason } => Some(format!("The sub-agent failed: {reason}")),
        Status::TurnLimitReached => Some(format!(
            "The sub-agent ran out of turns before it answered. The last it said: {}",
            last()
        )),
        Status::Yielded => Some(last()),
        Status::Waiting { .. } => None,
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
