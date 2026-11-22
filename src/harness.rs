//! Assembling the pieces into an agent, and the loop that runs it.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::error::Error;
use core::fmt;
#[cfg(feature = "std")]
use std::sync::{Mutex, MutexGuard, PoisonError};

use corduroy_core::call::Serve;
use corduroy_core::sync::Lock;
use corduroy_core::{
    App, ContextError, EventStream, FiberId, GATES, Gates, HTTP, HttpClients, LogError, MODELS,
    Message, Models, PROMPTS, Plugin, Prompts, Registry, Role, SECRETS, SERVICES, Secrets,
    Services, SessionId, TOOLS, ToolCall, ToolResult, Tools,
};
#[cfg(feature = "std")]
use corduroy_core::{KERNELS, Kernels};

use crate::children::{SESSIONS, SessionsService};
use crate::compact::CompactOperation;
use crate::gates::GateOperation;
use crate::loops::{DEFAULT_LOOP, LOOPS, Loops, Run, StandardLoop, Step};
use crate::operation::{Next, Operation};
use crate::provider::{DEFAULT_PROVIDER, ModelRequest, PROVIDERS, Provider, Providers};
use crate::session::{
    Answer, Ask, Effect, End, Journal, MAIN, Progress, Session, SessionInfo, Sessions, Wait, ended,
};
use crate::stop::Stop;
use crate::tools::{NOT_RUN, PluginTools};

/// Name of the service under which the harness provides the [`Operations`] registry.
pub const OPERATIONS: &str = "operations";

/// Steps of the loop that native plugins contribute.
///
/// A step is an [`Operation`]: it looks at the session at the start of a step of the loop
/// and either passes or returns [`Effect`]s for the journal to store. Registered steps are
/// tried in name order, after those the host added with [`Harness::with_operation`] and
/// ahead of the built-in ones, so one can act before tools run or the model is asked. A step
/// registered through a plugin's context leaves with the plugin.
///
/// Operations are native code and run with the host's authority; a WebAssembly plugin
/// shapes the loop through gates, tools and events instead.
pub type Operations = Registry<dyn Operation>;

/// Why the harness could not be set up or a run could not complete.
#[derive(Debug)]
pub enum HarnessError {
    /// The plugin tree rejected a service or plugin.
    Context(ContextError),
    /// The agent loop failed in a way no step recovered from.
    Run(String),
    /// The event log could not store a commit. Nothing after it was recorded or shown.
    Log(LogError),
    /// No session has this id.
    UnknownSession(SessionId),
    /// A session with this id already exists.
    DuplicateSession(SessionId),
    /// The session has a run that never reached its end, so it cannot take a new prompt.
    /// [`Harness::continue_run`] finishes it, and [`Harness::stop`] ends it.
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
                write!(
                    f,
                    "session `{id}` has an unfinished run: continue or stop it first"
                )
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
#[non_exhaustive]
pub enum Status {
    /// The model answered without asking for anything further.
    Finished {
        /// The model's final answer.
        answer: String,
    },
    /// The model was still working when it ran out of turns.
    TurnLimitReached,
    /// The model could not be reached or failed. The reason is in the run's `turn/end`.
    Failed {
        /// What went wrong.
        reason: String,
    },
    /// A person stopped the run with [`Harness::stop`].
    Stopped,
    /// The loop stopped with nothing left to do but no answer, for example because an
    /// operation stopped it.
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
            Self::Stopped => f.write_str("stopped"),
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

/// What one step of the loop did. See [`Harness::step`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Stepped {
    /// The step of this name did something, and the run goes on.
    Applied(String),
    /// The run stopped: it ended, or it waits.
    Stopped,
}

/// An agent: a plugin tree, an event stream and a programmable loop around a model.
///
/// A harness holds any number of sessions, each a conversation of its own on the one stream.
/// It starts with the session [`MAIN`], which [`run`](Self::run) uses.
///
/// The harness itself does very little. It provides the registries plugins contribute to
/// ([`Tools`], [`Prompts`], the kernels (with `std`), [`Gates`],
/// [`Services`], [`Providers`], [`Models`], [`Secrets`], [`HttpClients`], [`Operations`],
/// [`Loops`]), journals the conversation to the event stream, and drives a run step by step
/// until it ends or waits. What each step does is the [`Loop`](crate::Loop)'s, looked up in
/// [`Loops`] on every step: the [`StandardLoop`] unless [`with_loop`](Self::with_loop) names
/// another. It tries the operations, compaction, the gates, the tools, the turn limit and the
/// model, in that order.
pub struct Harness {
    /// Shared with the step that applies changes plugins asked for while the loop runs.
    #[cfg(feature = "std")]
    app: Arc<Mutex<App>>,
    #[cfg(not(feature = "std"))]
    app: Arc<Lock<App>>,
    events: Arc<EventStream>,
    tools: Arc<Tools>,
    prompts: Arc<Prompts>,
    #[cfg(feature = "std")]
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
    model: String,
    system: String,
    max_turns: u32,
    restart_limit: u32,
    /// The name of the loop that takes each step, looked up in `loops` on every step.
    looping: String,
    /// The loops a harness can run, by name.
    loops: Arc<Loops>,
    /// Steps the host added, tried first and in the order given.
    operations: Vec<Arc<dyn Operation>>,
    /// Steps plugins registered, tried next and in name order.
    contributed: Arc<Operations>,
    /// The sessions being run now, each with what stops it.
    running: Lock<BTreeMap<SessionId, Stop>>,
}

impl fmt::Debug for Harness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Harness")
            .field("tools", &self.tools)
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("max_turns", &self.max_turns)
            .finish_non_exhaustive()
    }
}

/// While alive, the session it names is being run, and [`Harness::stop`] reaches it.
struct Running<'harness> {
    running: &'harness Lock<BTreeMap<SessionId, Stop>>,
    session: SessionId,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let stop = self.running.with(|running| running.remove(&self.session));
        drop(stop);
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
        #[cfg(feature = "std")]
        let kernels = Arc::new(Kernels::new());
        let gates = Arc::new(Gates::new());
        let services = Arc::new(Services::new());
        let providers = Arc::new(Providers::new());
        let models = Arc::new(Models::new());
        let secrets = Arc::new(Secrets::new());
        let http = Arc::new(HttpClients::new());
        let contributed = Arc::new(Operations::new());
        let loops = Arc::new(Loops::new());
        Self::register_own(&loops, &services, &sessions)?;
        let mut app = App::with_events(Arc::clone(&events));
        app.provide(TOOLS, Arc::clone(&tools))?;
        app.provide(PROMPTS, Arc::clone(&prompts))?;
        #[cfg(feature = "std")]
        app.provide(KERNELS, Arc::clone(&kernels))?;
        app.provide(GATES, Arc::clone(&gates))?;
        app.provide(SERVICES, Arc::clone(&services))?;
        app.provide(PROVIDERS, Arc::clone(&providers))?;
        app.provide(MODELS, Arc::clone(&models))?;
        app.provide(SECRETS, Arc::clone(&secrets))?;
        app.provide(HTTP, Arc::clone(&http))?;
        app.provide(OPERATIONS, Arc::clone(&contributed))?;
        app.provide(LOOPS, Arc::clone(&loops))?;
        Ok(Self {
            #[cfg(feature = "std")]
            app: Arc::new(Mutex::new(app)),
            #[cfg(not(feature = "std"))]
            app: Arc::new(Lock::new(app)),
            events,
            tools,
            prompts,
            #[cfg(feature = "std")]
            kernels,
            gates,
            services,
            providers,
            models,
            secrets,
            http,
            sessions,
            provider: DEFAULT_PROVIDER.to_owned(),
            model: "default".to_owned(),
            system: String::new(),
            max_turns: 25,
            looping: DEFAULT_LOOP.to_owned(),
            loops,
            restart_limit: 5,
            operations: Vec::new(),
            contributed,
            running: Lock::new(BTreeMap::new()),
        })
    }

    /// What the harness registers itself, as the host rather than as a plugin: the
    /// [`StandardLoop`] as [`DEFAULT_LOOP`], and the [`SESSIONS`] service.
    fn register_own(
        loops: &Loops,
        services: &Services,
        sessions: &Arc<Sessions>,
    ) -> Result<(), HarnessError> {
        let duplicate =
            |duplicate: corduroy_core::Duplicate| HarnessError::Run(duplicate.to_string());
        loops
            .insert(DEFAULT_LOOP, Arc::new(StandardLoop))
            .map_err(duplicate)?;
        let children: Arc<dyn corduroy_core::Service> = Arc::new(SessionsService {
            sessions: Arc::clone(sessions),
        });
        services.insert(SESSIONS, children).map_err(duplicate)
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
        model.clone_into(&mut self.model);
        self
    }

    /// Start every system prompt with `text`. Plugins' prompt parts follow it.
    #[must_use]
    pub fn with_system_prompt(mut self, text: impl Into<String>) -> Self {
        self.system = text.into();
        self
    }

    /// Take each step with the loop registered as `name` in [`Loops`], rather than
    /// [`DEFAULT_LOOP`] (the [`StandardLoop`]). The loop is looked up on every step, so one a
    /// plugin registers takes over from the next step on, and unloading it ends the run as
    /// failed at its next step.
    #[must_use]
    pub fn with_loop(mut self, name: impl Into<String>) -> Self {
        self.looping = name.into();
        self
    }

    /// Stop a run after this many model turns without an answer. A loop reads it through
    /// [`max_turns`](Self::max_turns); the [`StandardLoop`] ends the run there.
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
    /// first that does something wins, so an operation added here can act before tools run
    /// or the model is called: approve calls, compact history, answer a command.
    ///
    /// This is for the host. A native plugin registers its steps in [`Operations`] instead
    /// ([`operations`](Self::operations)), so that they leave with it.
    #[must_use]
    pub fn with_operation(mut self, operation: Arc<dyn Operation>) -> Self {
        self.operations.push(operation);
        self
    }

    /// Load a plugin.
    pub fn plugin(&self, plugin: impl Plugin + 'static) -> FiberId {
        self.with_app(|app| app.plugin(plugin))
    }

    /// The plugin tree, for unloading and reloading plugins. Changes plugins ask for through
    /// [`Requests`](corduroy_core::Requests) are applied before each step of the loop, and by
    /// any change made here.
    #[cfg(feature = "std")]
    pub fn app(&self) -> MutexGuard<'_, App> {
        self.app.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Run `work` with the plugin tree, as [`app`](Self::app) does, in a build with or
    /// without the standard library. Without it the tree is held in a critical section, so
    /// keep `work` short.
    pub fn with_app<R>(&self, work: impl FnOnce(&mut App) -> R) -> R {
        #[cfg(feature = "std")]
        {
            work(&mut self.app())
        }
        #[cfg(not(feature = "std"))]
        {
            self.app.with(work)
        }
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

    /// The kernels plugins have registered. Kernels need the `std` feature.
    #[cfg(feature = "std")]
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

    /// The loops a run can take its steps with, by name. See [`with_loop`](Self::with_loop).
    #[must_use]
    pub const fn loops(&self) -> &Arc<Loops> {
        &self.loops
    }

    /// How many model turns a run may take without an answer.
    #[must_use]
    pub const fn max_turns(&self) -> u32 {
        self.max_turns
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

    /// The journal that records `session` on the event stream.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::UnknownSession`] if there is no such session, or
    /// [`HarnessError::Run`] if its events cannot be decoded.
    pub fn journal(&self, session: &SessionId) -> Result<Arc<Journal>, HarnessError> {
        self.sessions.journal(session)
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
    /// model answers, runs out of turns, fails or is stopped.
    ///
    /// Tool failures and model failures are recorded in the log, not returned from here:
    /// see [`Status`].
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::UnknownSession`] if there is no such session,
    /// [`HarnessError::Waiting`] if its last run waits for an answer,
    /// [`HarnessError::Unfinished`] if its last run never reached its end otherwise,
    /// [`HarnessError::Run`] if a step fails outright, or [`HarnessError::Log`] if the event
    /// log could not store a step.
    pub async fn run_in(&self, session: &SessionId, prompt: &str) -> Result<Outcome, HarnessError> {
        let journal = self.journal(session)?;
        if !journal.waiting().is_empty() || !journal.awaiting().is_empty() {
            return Err(HarnessError::Waiting(session.clone()));
        }
        if journal.unfinished() {
            return Err(HarnessError::Unfinished(session.clone()));
        }
        journal.record(&Message::user().text(prompt))?;
        self.drive(&journal, &Stop::default()).await
    }

    /// Ask the run of `session` to stop, and return whether there was one to stop.
    ///
    /// A run that is going (on another task or thread) stops at the next step: a tool that
    /// is running is told through its call scope ([`corduroy_core::call::stopping`]) and
    /// finishes early if it can, the calls of the step not yet begun are answered as not
    /// run, a model request in flight is abandoned, and the child sessions the run is
    /// running stop too. The run then ends as [`Status::Stopped`], and the session takes a
    /// new prompt; the model hears, through the results, which calls never ran.
    ///
    /// A run that is not going but has not ended (it waits for an answer or a signal, or a
    /// crash cut it short) is ended here, the same way, along with the child sessions it
    /// waits for. Calls the log shows finished keep their results.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::UnknownSession`] if there is no such session, or
    /// [`HarnessError::Log`] if the stop could not be recorded.
    pub fn stop(&self, session: &SessionId) -> Result<bool, HarnessError> {
        let journal = self.journal(session)?;
        let running = self.running.with(|running| running.get(session).cloned());
        if let Some(stop) = running {
            journal.stop_asked()?;
            stop.ask();
            return Ok(true);
        }
        let open =
            journal.unfinished() || !journal.waiting().is_empty() || !journal.awaiting().is_empty();
        if !open {
            return Ok(false);
        }
        journal.stop_asked()?;
        self.end_stopped(&journal)?;
        Ok(true)
    }

    /// End the run of `journal`'s session, and those of the children it waits for, as
    /// stopped.
    fn end_stopped(&self, journal: &Journal) -> Result<(), HarnessError> {
        for wait in journal.awaiting() {
            if let Some(child) = ended(&wait.signal)
                && let Ok(child) = self.journal(&child)
                && (child.unfinished() || !child.awaiting().is_empty())
            {
                self.end_stopped(&child)?;
            }
            let reason = "stopped: the run was stopped before this call finished";
            journal.signal(&wait.signal, reason)?;
        }
        if let Some(results) = self.settled(journal, NOT_RUN) {
            journal.record(&results)?;
        }
        journal.end_turn(&End::Stopped)?;
        Ok(())
    }

    /// Run one step of the loop for `session`: the step that applies first does its work,
    /// and its effects are stored. Returns what it did. A step that ends the run records
    /// the end.
    ///
    /// This is the loop [`run_in`](Self::run_in) runs, one step at a time, for a caller who
    /// wants to watch or interleave it. Record the person's message first with the session's
    /// [`journal`](Self::journal).
    ///
    /// # Errors
    ///
    /// As [`run_in`](Self::run_in).
    pub async fn step(&self, session: &SessionId) -> Result<Stepped, HarnessError> {
        let journal = self.journal(session)?;
        let stop = self
            .running
            .with(|running| running.get(session).cloned())
            .unwrap_or_default();
        Ok(match self.step_once(&journal, &stop).await? {
            Step::Applied(name) => Stepped::Applied(name),
            Step::Wait => Stepped::Stopped,
            Step::End(end) => {
                journal.end_turn(&end)?;
                Stepped::Stopped
            }
        })
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
    ///   [`replay_safe`](corduroy_core::ToolSpec::replay_safe). Otherwise it is answered
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
        self.continue_with(session, &Stop::default()).await
    }

    async fn continue_with(
        &self,
        session: &SessionId,
        stop: &Stop,
    ) -> Result<Outcome, HarnessError> {
        let journal = self.journal(session)?;
        if !journal.awaiting().is_empty() && journal.waiting().is_empty() {
            // A crash after a call began waiting but before its placeholder reached the
            // conversation leaves the call unanswered: answer it from the log, or the tools
            // step would make it again (and start a second child session).
            if let Some(settled) = self.settled(&journal, "") {
                journal.record(&settled)?;
            }
            return self.drive(&journal, stop).await;
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
            journal.end_turn(&End::Failed(reason))?;
            return Ok(self.outcome_of(&journal));
        }
        if let Some(settled) = self.settled(&journal, "") {
            journal.record(&settled)?;
        }
        self.drive(&journal, stop).await
    }

    /// Results for the unanswered tool calls of `journal`'s session that must not simply be
    /// run: those the log already has a result for, and those that were started and are
    /// not safe to make again. A call that never started is left for the tools step to
    /// run, unless `not_run` says what to answer it with instead.
    fn settled(&self, journal: &Journal, not_run: &str) -> Option<Message> {
        let mut results = Message::user();
        for call in journal.session().pending_calls() {
            let result = match journal.progress(&call.id) {
                Progress::NotStarted if not_run.is_empty() => continue,
                Progress::NotStarted => ToolResult::failed(call.id, not_run),
                Progress::Started if not_run.is_empty() && self.replay_safe(&call.name) => {
                    continue;
                }
                Progress::Started => ToolResult::failed(
                    call.id,
                    format!(
                        "interrupted: the harness stopped while `{}` was running, and it is \
                         not safe to run twice. Check what it did before calling it again.",
                        call.name
                    ),
                ),
                Progress::Finished { ok, output } => ToolResult::new(call.id, ok, output),
            };
            results = results.result(result);
        }
        (!results.is_empty()).then_some(results)
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
        let stop = Stop::default();
        let outcome = self.drive(&journal, &stop).await?;
        self.resume_parents(session, outcome, &stop).await
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
        let stop = Stop::default();
        let outcome = self.drive(&journal, &stop).await?;
        self.resume_parents(session, outcome, &stop).await
    }

    /// Once `session` has ended, take on each session up the chain that waits for it.
    async fn resume_parents(
        &self,
        session: &SessionId,
        mut outcome: Outcome,
        stop: &Stop,
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
            outcome = Box::pin(self.drive(&journal, stop)).await?;
            current = parent;
        }
    }

    fn replay_safe(&self, tool: &str) -> bool {
        self.tools
            .get(tool)
            .is_some_and(|tool| tool.spec().replay_safe)
    }

    /// Note that `session` is being run, stopped by `stop`, until the guard is dropped.
    fn running_as(&self, session: &SessionId, stop: &Stop) -> Running<'_> {
        let earlier = self
            .running
            .with(|running| running.insert(session.clone(), stop.clone()));
        drop(earlier);
        Running {
            running: &self.running,
            session: session.clone(),
        }
    }

    /// Run the loop over `journal`'s session until it ends or waits, run the child sessions
    /// it then waits for, and go on once they end.
    async fn drive(&self, journal: &Arc<Journal>, stop: &Stop) -> Result<Outcome, HarnessError> {
        let running = self.running_as(journal.id(), stop);
        loop {
            match self.step_once(journal, stop).await? {
                Step::Applied(_) => continue,
                Step::Wait => {}
                Step::End(end) => journal.end_turn(&end)?,
            }
            let awaiting = journal.awaiting();
            if awaiting.is_empty() || !self.run_children(journal, &awaiting, stop).await? {
                drop(running);
                return Ok(self.outcome_of(journal));
            }
        }
    }

    /// One step of the loop over `journal`'s session.
    ///
    /// What every loop gets, whichever it is: the changes plugins asked for are applied, and
    /// a run asked to stop ends as stopped. Then the loop registered under the harness's
    /// loop name in [`Loops`] takes the step.
    async fn step_once(&self, journal: &Arc<Journal>, stop: &Stop) -> Result<Step, HarnessError> {
        // Plugins loaded or unloaded while the loop ran take effect before the next step.
        self.with_app(App::apply_requests);
        if stop.asked() {
            return self.stopped(journal);
        }
        let Some(chosen) = self.loops.get(&self.looping) else {
            let reason = format!("no loop is registered as `{}`", self.looping);
            return Ok(Step::End(End::Failed(reason)));
        };
        chosen.step(&Run::new(self, journal, stop)).await
    }

    /// The operations of the standard order: the host's, then those plugins registered, then
    /// compaction and the gates.
    pub(crate) fn standard_operations(&self) -> Vec<Arc<dyn Operation>> {
        let builtin: [Arc<dyn Operation>; 2] = [
            Arc::new(CompactOperation::new(
                Arc::clone(&self.services),
                Arc::clone(&self.sessions),
            )),
            Arc::new(GateOperation::new(
                Arc::clone(&self.gates),
                Arc::clone(&self.tools),
                Arc::clone(&self.sessions),
            )),
        ];
        self.operations
            .iter()
            .cloned()
            .chain(self.contributed.entries())
            .chain(builtin)
            .collect()
    }

    /// Try `operations` in order on `session`; the first that does something is the step,
    /// and its effects are stored. `None` if every one passed.
    pub(crate) async fn operate(
        journal: &Journal,
        session: &Session,
        operations: Vec<Arc<dyn Operation>>,
    ) -> Result<Option<Step>, HarnessError> {
        for operation in operations {
            match operation.run(session).await? {
                Next::Pass => {}
                Next::Continue(effects) => {
                    journal.apply(effects)?;
                    return Ok(Some(Step::Applied(operation.name().to_owned())));
                }
                Next::Stop(effects) => {
                    journal.apply(effects)?;
                    return Ok(Some(Self::halted(journal)));
                }
            }
        }
        Ok(None)
    }

    /// Run `calls` from the [`Tools`] registry, each with its trail in the log, and store
    /// their results as one commit.
    pub(crate) async fn call_tools(
        &self,
        journal: &Arc<Journal>,
        calls: Vec<ToolCall>,
        stop: &Stop,
    ) -> Result<Step, HarnessError> {
        let tools = PluginTools::new(Arc::clone(&self.tools));
        let results = tools.run(journal, calls, stop).await?;
        journal.apply(vec![Effect::from(results)])?;
        Ok(Step::Applied("tools".to_owned()))
    }

    /// How a run whose last step stopped it stands: waiting, if a call waits for a person
    /// or a signal, and otherwise at its end.
    fn halted(journal: &Journal) -> Step {
        if journal.waiting().is_empty() && journal.awaiting().is_empty() {
            Step::End(End::settled(&journal.session()))
        } else {
            Step::Wait
        }
    }

    /// End a run that was asked to stop: the calls of its turn that have no result are
    /// answered (from the log if they finished, as not run if they did not begin).
    fn stopped(&self, journal: &Journal) -> Result<Step, HarnessError> {
        if let Some(results) = self.settled(journal, NOT_RUN) {
            journal.record(&results)?;
        }
        Ok(Step::End(End::Stopped))
    }

    /// The request the harness makes of the model for `session`: its model, the system
    /// prompt, the conversation as the model sees it, and every tool.
    pub(crate) fn request_for(&self, session: &Session) -> ModelRequest {
        ModelRequest::new(
            &self.model,
            self.system_prompt(session.id()),
            session.for_model(),
        )
        .with_tools(self.tools.specs())
    }

    /// Ask the model `request`, and record its answer and what it cost. A person stopping
    /// the run does not wait for the model to finish.
    pub(crate) async fn ask_model(
        &self,
        journal: &Arc<Journal>,
        request: &ModelRequest,
        stop: &Stop,
    ) -> Result<Step, HarnessError> {
        let Some(provider) = self.providers.get(&self.provider) else {
            let reason = format!("no model provider is registered as `{}`", self.provider);
            return Ok(Step::End(End::Failed(reason)));
        };
        let Some(reply) = stop.unless_asked(provider.complete(request)).await else {
            return self.stopped(journal);
        };
        match reply {
            Ok(reply) => {
                let mut message = reply.message;
                message.role = Role::Assistant;
                let usage = Effect::Usage {
                    model: request.model.clone(),
                    usage: reply.usage,
                };
                journal.apply(vec![Effect::from(message), usage])?;
                Ok(Step::Applied("model".to_owned()))
            }
            Err(error) => Ok(Step::End(End::Failed(error.to_string()))),
        }
    }

    /// The system prompt for a request of `session`: the host's text, then each plugin's
    /// part, rendered for that session.
    fn system_prompt(&self, session: &SessionId) -> String {
        let (parts, _) = Serve::session(session.clone()).run(|| self.prompts.render());
        let parts = parts.into_iter().map(|(_, text)| text);
        let system = (!self.system.is_empty()).then(|| self.system.clone());
        system
            .into_iter()
            .chain(parts)
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Run each child session `awaiting` waits for, and bring the end of those that ended
    /// to `journal`. Returns whether any did.
    async fn run_children(
        &self,
        journal: &Journal,
        awaiting: &[Wait],
        stop: &Stop,
    ) -> Result<bool, HarnessError> {
        let mut resolved = false;
        for wait in awaiting {
            let Some(child) = ended(&wait.signal) else {
                continue;
            };
            if let Some(payload) = Box::pin(self.run_child(&child, stop)).await? {
                resolved |= journal.signal(&wait.signal, &payload)?;
            }
        }
        Ok(resolved)
    }

    /// Take the child session `child` as far as it goes. Returns what its end brings to the
    /// call waiting for it, or `None` if it waits itself.
    async fn run_child(
        &self,
        child: &SessionId,
        stop: &Stop,
    ) -> Result<Option<String>, HarnessError> {
        let journal = self.journal(child)?;
        let started = !journal.session().conversation().is_empty();
        let outcome = if !started {
            let task = self
                .session_list()
                .into_iter()
                .find(|info| info.id == *child)
                .and_then(|info| info.task)
                .unwrap_or_default();
            journal.record(&Message::user().text(task))?;
            Box::pin(self.drive(&journal, stop)).await?
        } else if journal.unfinished() || !journal.awaiting().is_empty() {
            Box::pin(self.continue_with(child, stop)).await?
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
            status_of(journal, &session)
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
            .iter()
            .rev()
            .map(Message::text_content)
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
        Status::Stopped => Some("The sub-agent was stopped before it finished.".to_owned()),
        Status::Yielded => Some(last()),
        Status::Waiting { .. } => None,
    }
}

/// How the last run of `session` ended, as `journal` recorded it.
fn status_of(journal: &Journal, session: &Session) -> Status {
    let answered = || {
        session
            .answer()
            .map_or(Status::Yielded, |answer| Status::Finished { answer })
    };
    match journal.ended() {
        Some(End::TurnLimit) => Status::TurnLimitReached,
        Some(End::Failed(reason)) => Status::Failed { reason },
        Some(End::Stopped) => Status::Stopped,
        Some(End::Yielded) => Status::Yielded,
        // A run that ended before runs recorded how is read from its conversation.
        Some(End::Finished) | None => answered(),
    }
}
