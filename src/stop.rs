//! Asking a run to stop: the flag a run, its sub-agents and its tools share. See
//! [`Harness::stop`](crate::Harness::stop).
//!
//! Waiting for a stop needs no particular executor: whoever waits leaves its waker, and
//! asking wakes them all.
//!
//! The same flag knows since when the run has been driven, where there is a clock to read
//! (the `std` feature), which is what a run's time limit is held against
//! ([`Harness::with_time_limit`](crate::Harness::with_time_limit)).

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::poll_fn;
use core::mem;
use core::pin::pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Poll, Waker};
use core::time::Duration;
#[cfg(feature = "std")]
use std::thread::{self, JoinHandle};
#[cfg(feature = "std")]
use std::time::Instant;

use corduroy_core::sync::Lock;

/// Shared by a run, the child sessions it runs and the tools it calls: once asked, each of
/// them finishes what it can and the run ends as stopped.
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "std"), derive(Default))]
pub(crate) struct Stop {
    flag: Arc<AtomicBool>,
    /// The tasks waiting in [`asked_now`](Self::asked_now), to wake when the run is asked.
    waiting: Arc<Lock<Vec<Waker>>>,
    /// When whoever drives the run took it on.
    #[cfg(feature = "std")]
    began: Instant,
}

#[cfg(feature = "std")]
impl Default for Stop {
    fn default() -> Self {
        Self {
            flag: Arc::default(),
            waiting: Arc::default(),
            began: Instant::now(),
        }
    }
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

impl Stop {
    /// Whether the run has been driven for `limit`, by whoever took it on with this stop.
    #[cfg(feature = "std")]
    pub(crate) fn driven_for(&self, limit: Duration) -> bool {
        self.began.elapsed() >= limit
    }

    /// Whether the run has been driven for `limit`: never, where there is no clock to read.
    #[cfg(not(feature = "std"))]
    pub(crate) const fn driven_for(&self, limit: Duration) -> bool {
        let _: (&Self, Duration) = (self, limit);
        false
    }

    /// Wait for `work`, unless the run has been driven for `limit` first: then `None`, and
    /// `work` is dropped unfinished. Without a limit, or without a clock, `work` is simply
    /// waited for.
    pub(crate) async fn within<T>(
        &self,
        limit: Option<Duration>,
        work: impl Future<Output = T>,
    ) -> Option<T> {
        #[cfg(feature = "std")]
        if let Some(alarm) = limit.and_then(|limit| Alarm::at(self.began.checked_add(limit)?)) {
            return alarm.rang.unless_asked(work).await;
        }
        #[cfg(not(feature = "std"))]
        let _: Option<Duration> = limit;
        Some(work.await)
    }
}

/// A thread that sleeps until a moment and then wakes whoever waits for it, on any
/// executor. Dropped first, it wakes nobody.
#[cfg(feature = "std")]
struct Alarm {
    /// Asked once the moment has come.
    rang: Stop,
    /// Set when nobody waits any more.
    dropped: Arc<AtomicBool>,
    sleeper: JoinHandle<()>,
}

#[cfg(feature = "std")]
impl Alarm {
    /// An alarm for `moment`. `None` if the thread could not be started.
    fn at(moment: Instant) -> Option<Self> {
        let rang = Stop::default();
        let dropped = Arc::new(AtomicBool::new(false));
        let (bell, gone) = (rang.clone(), Arc::clone(&dropped));
        let sleeper = thread::Builder::new()
            .name("corduroy-time-limit".into())
            .spawn(move || sleep_until(moment, &gone, &bell))
            .ok()?;
        Some(Self {
            rang,
            dropped,
            sleeper,
        })
    }
}

/// Sleep until `moment` and then ask `bell`, unless `gone` is set first.
#[cfg(feature = "std")]
fn sleep_until(moment: Instant, gone: &AtomicBool, bell: &Stop) {
    while !gone.load(Ordering::SeqCst) {
        let left = moment.saturating_duration_since(Instant::now());
        if left.is_zero() {
            bell.ask();
            return;
        }
        thread::park_timeout(left);
    }
}

#[cfg(feature = "std")]
impl Drop for Alarm {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
        self.sleeper.thread().unpark();
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

    #[tokio::test]
    async fn work_that_outlasts_the_time_left_is_dropped() {
        let stop = Stop::default();
        let limit = Some(Duration::from_millis(20));

        let never = stop.within(limit, core::future::pending::<()>()).await;

        assert_eq!(never, None);
        assert!(stop.driven_for(Duration::from_millis(20)));
        // Work that is done in time is waited for, and so is any work without a limit.
        let stop = Stop::default();
        let hour = Some(Duration::from_secs(3600));
        assert_eq!(stop.within(hour, async { 1 }).await, Some(1));
        assert_eq!(stop.within(None, async { 2 }).await, Some(2));
    }
}
