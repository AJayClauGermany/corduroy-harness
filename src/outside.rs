//! What something outside a run may do to one, and what a person reaches the agent through
//! besides the host.
//!
//! A run is driven by the host that holds the [`Harness`](crate::Harness). An extension
//! that stands between a person and the runs (a page in a browser, say) holds no harness
//! and runs no loop. It *records*, through [`Outside`], and returns; the host's driver then
//! takes the run on ([`Harness::answered`](crate::Harness::answered)). So there is one
//! driver however many such extensions there are, and nothing is lost if the process ends
//! between the two: what was recorded is in the log.
//!
//! Such an extension is a [`Front`]. It registers in [`Fronts`], and the host opens each
//! one when it is about to drive runs, not when the agent is merely put together to be
//! looked at.
//!
//! Both are for extensions only. They are provided to native plugins by name ([`OUTSIDE`],
//! [`FRONTS`]) and are not in the [`Services`](corduroy_core::Services) registry, so no
//! add-on reaches them: a gate's question is the person's to answer, and an add-on that
//! could answer one would hold more than it was granted.
//!
//! Of what the design gives something outside a run to do, this is the part a front end
//! needs first: reading the questions that wait, and answering one.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use corduroy_core::{Registry, SessionId};

use crate::harness::HarnessError;
use crate::session::{Answer, Ask, Sessions};

/// Name under which the harness provides [`Outside`] to native plugins.
pub const OUTSIDE: &str = "outside";

/// Name under which the harness provides the [`Fronts`] registry to native plugins.
pub const FRONTS: &str = "fronts";

/// What an extension may do to runs from outside them. See the [module](self).
pub struct Outside {
    sessions: Arc<Sessions>,
}

impl core::fmt::Debug for Outside {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Outside").finish_non_exhaustive()
    }
}

impl Outside {
    pub(crate) const fn new(sessions: Arc<Sessions>) -> Self {
        Self { sessions }
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
    /// it on ([`Harness::answered`](crate::Harness::answered)), in this process or a later
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
        if journal.answer(call, answer, Some(via))? {
            Ok(())
        } else {
            Err(HarnessError::NotAsked {
                session: session.clone(),
                call: call.into(),
            })
        }
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
