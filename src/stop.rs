//! Asking a run to stop: the flag a run, its sub-agents and its tools share. See
//! [`Harness::stop`](crate::Harness::stop).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

/// Shared by a run, the child sessions it runs and the tools it calls: once asked, each of
/// them finishes what it can and the run ends as stopped.
#[derive(Debug, Clone, Default)]
pub(crate) struct Stop {
    flag: Arc<AtomicBool>,
    asked: Arc<Notify>,
}

impl Stop {
    /// Ask the run to stop. Whatever waits in [`asked_now`](Self::asked_now) wakes.
    pub(crate) fn ask(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.asked.notify_waiters();
    }

    /// Whether the run was asked to stop.
    pub(crate) fn asked(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// The flag tools are given, through their call scope
    /// ([`corduroy_core::call::stopping`]).
    pub(crate) fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }

    /// Wait until the run is asked to stop; at once if it already was.
    pub(crate) async fn asked_now(&self) {
        // Registered before the flag is read, so an ask between the two is not missed.
        let asked = self.asked.notified();
        if self.asked() {
            return;
        }
        asked.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn waiting_for_a_stop_wakes_when_it_is_asked() {
        let stop = Stop::default();
        let asker = stop.clone();
        let waiting = tokio::spawn(async move { stop.asked_now().await });
        tokio::task::yield_now().await;

        asker.ask();

        waiting.await.unwrap();
        assert!(asker.asked());
        // Once asked, waiting returns at once.
        asker.asked_now().await;
    }
}
