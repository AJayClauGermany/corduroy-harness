//! What something outside a run may do to one, and what a person reaches the agent through
//! besides the host.
//!
//! A run is driven by the host that holds the [`Harness`](crate::Harness). An extension
//! that stands between a person and the runs (a page in a browser, say) holds no harness
//! and runs no loop. It *records*, through [`Outside`], and returns; the host's driver then
//! takes the run on ([`Harness::take_on`](crate::Harness::take_on)). So there is one
//! driver however many such extensions there are, and nothing is lost if the process ends
//! between the two: what was recorded is in the log.
//!
//! Such an extension is a [`Front`]. It registers in [`Fronts`], and the host opens each
//! one when it is about to drive runs, not when the agent is merely put together to be
//! looked at.
//!
//! What is recorded is heard at once by whoever listens to the stream, the host among them,
//! while the call that records it has not yet returned. So neither side acts from inside a
//! listener: the host does not take a run on there (it would wait for the recording it is
//! being told of), and an extension does not record there. A listener wakes something, and
//! that acts.
//!
//! Both are for extensions only. They are provided to native plugins by name ([`OUTSIDE`],
//! [`FRONTS`]) and are not in the [`Services`](corduroy_core::Services) registry, so no
//! add-on reaches them: a gate's question is the person's to answer, and an add-on that
//! could answer one would hold more than it was granted.
//!
//! Of what the design gives something outside a run to do, this is the part a front end
//! needs first: reading the questions that wait, answering one, saying something to a
//! session that is doing nothing, and stopping a run.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use corduroy_core::sync::Lock;
use corduroy_core::{Message, Registry, SessionId};

use crate::harness::{HarnessError, settled};
use crate::live::{Live, Watch};
use crate::session::{Answer, Ask, End, Journal, Sessions, ended};
use crate::stop::Stop;
use crate::tools::NOT_RUN;

/// The runs under way, and how one is stopped: shared by the harness, which runs them, and
/// by [`Outside`], through which an extension asks for a stop.
pub(crate) struct Runs {
    sessions: Arc<Sessions>,
    /// The sessions being run now, each with what stops it.
    pub(crate) running: Lock<BTreeMap<SessionId, Stop>>,
}

impl Runs {
    const fn new(sessions: Arc<Sessions>) -> Self {
        Self {
            sessions,
            running: Lock::new(BTreeMap::new()),
        }
    }

    /// Ask the run of `session` to stop, as [`Harness::stop`](crate::Harness::stop) says,
    /// recording which plugin the stop was given through, if it was not the host itself.
    pub(crate) fn stop(
        &self,
        session: &SessionId,
        via: Option<&str>,
    ) -> Result<bool, HarnessError> {
        let journal = self.sessions.journal(session)?;
        let running = self.running.with(|running| running.get(session).cloned());
        if let Some(stop) = running {
            journal.stop_asked(via)?;
            stop.ask();
            return Ok(true);
        }
        let open =
            journal.unfinished() || !journal.waiting().is_empty() || !journal.awaiting().is_empty();
        if !open {
            return Ok(false);
        }
        journal.stop_asked(via)?;
        self.end_stopped(&journal)?;
        Ok(true)
    }

    /// End the run of `journal`'s session, and those of the children it waits for, as
    /// stopped.
    fn end_stopped(&self, journal: &Journal) -> Result<(), HarnessError> {
        for wait in journal.awaiting() {
            if let Some(child) = ended(&wait.signal)
                && let Ok(child) = self.sessions.journal(&child)
                && (child.unfinished() || !child.awaiting().is_empty())
            {
                self.end_stopped(&child)?;
            }
            let reason = "stopped: the run was stopped before this call finished";
            journal.signal(&wait.signal, reason)?;
        }
        // Nothing is run again here, so whether a tool could be is not asked.
        if let Some(results) = settled(journal, NOT_RUN, &|_| false) {
            journal.record(&results)?;
        }
        journal.end_turn(&End::Stopped)?;
        Ok(())
    }
}

/// Name under which the harness provides [`Outside`] to native plugins.
pub const OUTSIDE: &str = "outside";

/// Name under which the harness provides the [`Fronts`] registry to native plugins.
pub const FRONTS: &str = "fronts";

/// What an extension may do to runs from outside them. See the [module](self).
pub struct Outside {
    sessions: Arc<Sessions>,
    /// The runs under way, which the harness that made this notes here.
    pub(crate) runs: Runs,
    /// What each session's model is writing, for whoever watches.
    pub(crate) live: Live,
    /// Whether the host takes on a message recorded here.
    heard: AtomicBool,
    /// Held while something is being recorded here. An event is stored, and heard by
    /// whoever listens, before the session's own account of itself has caught up with it;
    /// a host woken by the event waits on this, and so finds the whole of it recorded.
    recording: Lock<()>,
}

impl core::fmt::Debug for Outside {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Outside").finish_non_exhaustive()
    }
}

impl Outside {
    pub(crate) fn new(sessions: Arc<Sessions>) -> Self {
        Self {
            runs: Runs::new(Arc::clone(&sessions)),
            live: Live::default(),
            sessions,
            heard: AtomicBool::new(false),
            recording: Lock::new(()),
        }
    }

    /// Wait until nothing is midway through being recorded here. The host calls this
    /// before it reads a session to take its run on.
    pub(crate) fn recorded(&self) {
        self.recording.with(|()| ());
    }

    /// Note whether the host takes on a message recorded through [`say`](Self::say).
    pub(crate) fn hears(&self, takes: bool) {
        self.heard.store(takes, Ordering::SeqCst);
    }

    /// Whether a message said to a session now would be run: whether the host said it takes
    /// such messages on ([`Harness::takes_messages`](crate::Harness::takes_messages)).
    #[must_use]
    pub fn is_heard(&self) -> bool {
        self.heard.load(Ordering::SeqCst)
    }

    /// Watch what the model of `session` writes while it writes it, or of every session
    /// where none is named: the [live channel](crate::live). What is shown is a preview for
    /// a person to read, never recorded and never to be acted on; the whole answer is the
    /// `message` event that follows.
    #[must_use]
    pub fn watch(&self, session: Option<&SessionId>) -> Watch {
        self.live.watch(session)
    }

    /// The tool calls a gate asked about that nobody has answered yet, in every session.
    #[must_use]
    pub fn asks(&self) -> Vec<Ask> {
        self.sessions
            .list()
            .into_iter()
            .filter_map(|info| self.sessions.journal(&info.id).ok())
            .flat_map(|journal| journal.waiting())
            .collect()
    }

    /// Record a person's answer to the gate that asked about `call` in `session`, given
    /// through the plugin `via` (the answer's event names it).
    ///
    /// The answer is recorded and nothing more: the run goes on when the host's driver takes
    /// it on ([`Harness::take_on`](crate::Harness::take_on)), in this process or a later
    /// one.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::UnknownSession`] if there is no such session,
    /// [`HarnessError::NotAsked`] if no gate of it waits for an answer about `call` (someone
    /// else may have answered first), or [`HarnessError::Log`] if the answer could not be
    /// recorded.
    pub fn answer(
        &self,
        via: &str,
        session: &SessionId,
        call: &str,
        answer: &Answer,
    ) -> Result<(), HarnessError> {
        let journal = self.sessions.journal(session)?;
        let answered = self
            .recording
            .with(|()| journal.answer(call, answer, Some(via)))?;
        if answered {
            Ok(())
        } else {
            Err(HarnessError::NotAsked {
                session: session.clone(),
                call: call.into(),
            })
        }
    }
}

impl Outside {
    /// Record `text` as what the person says to `session`, given through the plugin `via`
    /// (the message's event names it), which begins a run of it.
    ///
    /// The message is recorded and nothing more: the host's driver takes the run on
    /// ([`Harness::take_on`](crate::Harness::take_on)). So it is taken only from a host that
    /// said it does that, and only for a session that is doing nothing: what the design
    /// calls the queue, a message kept for a session that is working, is not built.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::Run`] if the host takes no messages from outside or `text`
    /// is empty, [`HarnessError::UnknownSession`] if there is no such session,
    /// [`HarnessError::Waiting`] or [`HarnessError::Unfinished`] if a run of it is under
    /// way, or [`HarnessError::Log`] if the message could not be recorded.
    pub fn say(&self, via: &str, session: &SessionId, text: &str) -> Result<(), HarnessError> {
        if !self.is_heard() {
            return Err(HarnessError::Run(
                "nobody is here to run it: the host runs one prompt and leaves".into(),
            ));
        }
        if text.trim().is_empty() {
            return Err(HarnessError::Run("there is nothing to say".into()));
        }
        let journal = self.sessions.journal(session)?;
        self.recording.with(|()| self.begin(&journal, text, via))
    }

    /// Record `text` as said to the session of `journal` through `via`, if it is doing
    /// nothing.
    fn begin(&self, journal: &Journal, text: &str, via: &str) -> Result<(), HarnessError> {
        let session = journal.id();
        if !journal.waiting().is_empty() || !journal.awaiting().is_empty() {
            return Err(HarnessError::Waiting(session.clone()));
        }
        let running = self
            .runs
            .running
            .with(|running| running.contains_key(session));
        if running || journal.unfinished() {
            return Err(HarnessError::Unfinished(session.clone()));
        }
        journal.record_from(&Message::user().text(text), Some(via))?;
        Ok(())
    }

    /// Ask the run of `session` to stop, through the plugin `via` (the stop's event names
    /// it), and return whether there was one to stop. As
    /// [`Harness::stop`](crate::Harness::stop): a run that is going stops at its next step,
    /// and one that waits is ended here, which is recording and no more.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::UnknownSession`] if there is no such session, or
    /// [`HarnessError::Log`] if the stop could not be recorded.
    pub fn stop(&self, via: &str, session: &SessionId) -> Result<bool, HarnessError> {
        self.recording.with(|()| self.runs.stop(session, Some(via)))
    }
}

/// Something other than the host's own terminal that a person reaches the agent through.
pub trait Front: Send + Sync {
    /// Begin, now that the host is about to drive runs, and say where the person finds it:
    /// one line, which the host shows them. The line is not recorded, so it may hold what
    /// lets them in.
    ///
    /// Opening one that is open says the same again.
    ///
    /// # Errors
    ///
    /// Why it could not begin, for the host to show in the line's place. The agent runs
    /// without it.
    fn open(&self) -> Result<String, String>;
}

/// The front ends of an agent, by the name of the plugin that registered each.
pub type Fronts = Registry<dyn Front>;
