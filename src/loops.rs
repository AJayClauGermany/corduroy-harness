//! The loop as a part that can be swapped: what one step of a run does.
//!
//! A run is a series of steps over one session. The harness decides *when* there is a step and
//! keeps what every run must keep, whichever loop takes it: before each step it applies the
//! changes plugins asked for, a run asked to stop ends as stopped, a run that waits for a
//! person or a signal waits in the log, child sessions run when their parent waits for them,
//! and how the run ended is recorded. A [`Loop`] decides *what* each step does: which
//! operations to try, when to run tools, when to ask the model and with what, and when the run
//! is over.
//!
//! Loops are registered by name in the [`Loops`] registry, which the harness provides as the
//! service named [`LOOPS`], and the harness asks the one named by
//! [`Harness::with_loop`](crate::Harness::with_loop) ([`DEFAULT_LOOP`] unless told otherwise)
//! on every step, as it asks a provider for every model request. So a plugin can contribute a
//! loop, and unloading the plugin takes it away. The harness registers [`StandardLoop`] as
//! [`DEFAULT_LOOP`].
//!
//! A loop is built from what [`Run`] offers: the session, the standard operations, running
//! tool calls with their trail in the log, and asking the model. Each of those records what it
//! did in the log, so a run any loop took can be read back, picked up after a crash and
//! stopped the same way.
//!
//! ```
//! use std::sync::Arc;
//! use corduroy_harness::scripted::{ScriptedProvider, say};
//! use corduroy_harness::{End, Harness, HarnessError, Loop, Run, Status, Step};
//!
//! /// Ask the model once, without tools, and take what it says.
//! struct Once;
//!
//! #[async_trait::async_trait]
//! impl Loop for Once {
//!     async fn step(&self, run: &Run<'_>) -> Result<Step, HarnessError> {
//!         let session = run.session();
//!         if session.model_turns() > 0 {
//!             return Ok(Step::End(End::settled(&session)));
//!         }
//!         run.ask_model(&run.request(&session).with_tools(Vec::new())).await
//!     }
//! }
//!
//! # tokio::runtime::Runtime::new()?.block_on(async {
//! let harness = Harness::new(Arc::new(ScriptedProvider::new([say("hello")])))?.with_loop("once");
//! harness.loops().insert("once", Arc::new(Once))?;
//!
//! let outcome = harness.run("greet me").await?;
//! assert_eq!(outcome.status, Status::Finished { answer: "hello".into() });
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! # })?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use async_trait::async_trait;
use corduroy_core::{Registry, Role, ToolCall};

use crate::harness::{Harness, HarnessError};
use crate::operation::Operation;
use crate::provider::ModelRequest;
use crate::session::{End, Journal, Session};
use crate::stop::Stop;

/// Name of the service under which the harness provides the [`Loops`] registry.
pub const LOOPS: &str = "loops";

/// The loop a harness takes its steps with unless told otherwise, and the name the harness
/// registers [`StandardLoop`] under.
pub const DEFAULT_LOOP: &str = "default";

/// The registry of every loop, by name.
pub type Loops = Registry<dyn Loop>;

/// What one step of a run came to.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Step {
    /// The part of this name did something (an operation, `tools`, `model`), and the run
    /// goes on.
    Applied(String),
    /// The run waits, in the log, for a person or a signal.
    Wait,
    /// The run is over, and this is how it ended.
    End(End),
}

/// What a step of a run does. See [the module](self).
///
/// A step records what it does in the log, through what [`Run`] offers or the session's
/// [`Journal`], and says what it came to. It never keeps the conversation anywhere else:
/// the next step, or the same run picked up after a crash, reads it from the log again.
#[async_trait]
pub trait Loop: Send + Sync {
    /// Take one step of the run `run` is for.
    ///
    /// # Errors
    ///
    /// An error is a broken pipeline, as an [`Operation`]'s is: the run stops and its
    /// caller gets it. A failure the model or a person should hear of ends the run with
    /// [`End::Failed`], or goes to the model as a message.
    async fn step(&self, run: &Run<'_>) -> Result<Step, HarnessError>;
}

/// One step of a run, as a [`Loop`] sees it: the session, and the parts of the harness a loop
/// is built from.
#[derive(Debug)]
pub struct Run<'run> {
    harness: &'run Harness,
    journal: &'run Arc<Journal>,
    stop: &'run Stop,
}

impl<'run> Run<'run> {
    pub(crate) const fn new(
        harness: &'run Harness,
        journal: &'run Arc<Journal>,
        stop: &'run Stop,
    ) -> Self {
        Self {
            harness,
            journal,
            stop,
        }
    }

    /// The harness: its registries and its settings.
    #[must_use]
    pub const fn harness(&self) -> &'run Harness {
        self.harness
    }

    /// The journal that records the session on the event stream.
    #[must_use]
    pub const fn journal(&self) -> &'run Arc<Journal> {
        self.journal
    }

    /// The session as it stands now.
    #[must_use]
    pub fn session(&self) -> Session {
        self.journal.session()
    }

    /// Whether a person asked the run to stop. The harness ends a run asked to stop before
    /// its next step; a loop that works long within one step can look sooner.
    #[must_use]
    pub fn stopping(&self) -> bool {
        self.stop.asked()
    }

    /// The operations in the standard order: those the host added
    /// ([`Harness::with_operation`]), those plugins registered in
    /// [`Operations`](crate::Operations), then compaction and the gates.
    #[must_use]
    pub fn operations(&self) -> Vec<Arc<dyn Operation>> {
        self.harness.standard_operations()
    }

    /// Try `operations` in order on `session`. The first that does something is the step:
    /// its effects are stored as one commit, and the step is
    /// [`Applied`](Step::Applied) under its name, or the run waits or ends if it said
    /// [`Stop`](crate::Next::Stop). `None` if every operation passed.
    ///
    /// # Errors
    ///
    /// Whatever an operation returns, or [`HarnessError::Log`] if its effects could not be
    /// stored.
    pub async fn operate(
        &self,
        session: &Session,
        operations: Vec<Arc<dyn Operation>>,
    ) -> Result<Option<Step>, HarnessError> {
        Harness::operate(self.journal, session, operations).await
    }

    /// Run `calls` from the [`Tools`](corduroy_core::Tools) registry, one after another, and
    /// store their results as one commit. Each call has its own trail in the log
    /// (`tool/start`, `tool/result`); a call whose tool waits for a signal makes the run wait;
    /// once the run is asked to stop, the calls not yet begun are answered as not run.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Log`] if the log refused a commit. A tool that fails is a result for
    /// the model, not an error.
    pub async fn call_tools(&self, calls: Vec<ToolCall>) -> Result<Step, HarnessError> {
        self.harness
            .call_tools(self.journal, calls, self.stop)
            .await
    }

    /// The request the harness would make of the model for `session`: its model, the system
    /// prompt with every plugin's part, the conversation as the model sees it, and every
    /// tool. Change it before [`ask_model`](Self::ask_model) to ask something else.
    #[must_use]
    pub fn request(&self, session: &Session) -> ModelRequest {
        self.harness.request_for(session)
    }

    /// Ask the model `request`, of the provider the harness asks, and store its answer and
    /// what it cost as one commit. A model that fails ends the run as
    /// [failed](End::Failed); a person stopping the run does not wait for the answer.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Log`] if the answer could not be stored.
    pub async fn ask_model(&self, request: &ModelRequest) -> Result<Step, HarnessError> {
        self.harness
            .ask_model(self.journal, request, self.stop)
            .await
    }
}

/// The loop a harness takes its steps with unless told otherwise.
///
/// At every step it tries, in order, and the first that does something is the step: the
/// [operations](Run::operations) (the host's, the plugins', compaction, the gates); the tool
/// calls the model asked for; waiting, if a call waits for a signal; ending the run, if the
/// model has answered; ending it at the [turn limit](Harness::max_turns); and asking the
/// model.
///
/// A loop of one's own can wrap this one, to watch or adjust its steps, or take its parts
/// from [`Run`] and order them differently.
#[derive(Debug, Clone, Copy, Default)]
pub struct StandardLoop;

#[async_trait]
impl Loop for StandardLoop {
    async fn step(&self, run: &Run<'_>) -> Result<Step, HarnessError> {
        let session = run.session();
        if let Some(step) = run.operate(&session, run.operations()).await? {
            return Ok(step);
        }
        let pending = session.pending_calls();
        if !pending.is_empty() {
            return run.call_tools(pending).await;
        }
        if !run.journal().awaiting().is_empty() {
            return Ok(Step::Wait);
        }
        let answered = session
            .conversation()
            .last()
            .is_none_or(|last| last.role == Role::Assistant);
        if answered {
            return Ok(Step::End(End::settled(&session)));
        }
        let limit = usize::try_from(run.harness().max_turns()).unwrap_or(usize::MAX);
        if session.model_turns() >= limit {
            return Ok(Step::End(End::TurnLimit));
        }
        run.ask_model(&run.request(&session)).await
    }
}

impl Step {
    /// The name of what did something, if the run goes on.
    #[must_use]
    pub fn applied(&self) -> Option<&str> {
        match self {
            Self::Applied(name) => Some(name),
            Self::Wait | Self::End(_) => None,
        }
    }
}
