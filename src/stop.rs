//! Asking a run to stop: the flag a run, its sub-agents and its tools share. See
//! [`Harness::stop`](crate::Harness::stop).
//!
//! Waiting for a stop needs no particular executor: whoever waits leaves its waker, and
//! asking wakes them all.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::poll_fn;
use core::mem;
use core::pin::pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Poll, Waker};

use corduroy_core::sync::Lock;

/// Shared by a run, the child sessions it runs and the tools it calls: once asked, each of
/// them finishes what it can and the run ends as stopped.
#[derive(Debug, Clone, Default)]
pub(crate) struct Stop {
    flag: Arc<AtomicBool>,
    /// The tasks waiting in [`asked_now`](Self::asked_now), to wake when the run is asked.
    waiting: Arc<Lock<Vec<Waker>>>,
}

impl Stop {
    /// Ask the run to stop. Whatever waits in [`asked_now`](Self::asked_now) wakes.
    pub(crate) fn ask(&self) {
        self.flag.store(true, Ordering::SeqCst);
        // Woken outside the lock: a waker may run code that looks at this stop again.
        for waker in self.waiting.with(mem::take) {
            waker.wake();
        }
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
        poll_fn(|cx| {
            if self.asked() {
                return Poll::Ready(());
            }
            self.waiting.with(|waiting| leave(waiting, cx.waker()));
            // Asked between the first look and leaving the waker: do not sleep through it.
            if self.asked() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
    }

    /// Wait for `work`, unless the run is asked to stop first: then `None`, and `work` is
    /// dropped unfinished.
    pub(crate) async fn unless_asked<T>(&self, work: impl Future<Output = T>) -> Option<T> {
        let mut asked = pin!(self.asked_now());
        let mut work = pin!(work);
        poll_fn(|cx| {
            if asked.as_mut().poll(cx).is_ready() {
                return Poll::Ready(None);
            }
            work.as_mut().poll(cx).map(Some)
        })
        .await
    }
}

/// Add `waker` to `waiting`, unless one there would wake the same task.
fn leave(waiting: &mut Vec<Waker>, waker: &Waker) {
    if !waiting.iter().any(|left| left.will_wake(waker)) {
        waiting.push(waker.clone());
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
