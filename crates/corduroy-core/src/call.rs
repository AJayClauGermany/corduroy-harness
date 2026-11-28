//! What a thread is being called for: the session it serves, the tool call if there is one,
//! the signals that call waits for, and whether the run was asked to stop.
//!
//! A tool runs synchronously on the thread that calls it, and so does everything it calls in
//! turn: a WebAssembly plugin's host functions, a service another plugin provides. Whoever
//! calls into plugins on behalf of a session says so with [`Serve`] (or [`within`], for a
//! tool call), and anything on that call chain can then ask which session and which call it
//! serves ([`current`]). That is how a plugin keeps something of its own for each session
//! (a list per session, a sub-agent's apart from its parent's), and how the events it
//! raises carry the session.
//!
//! Three things use the scope:
//!
//! * **Session.** A tool call, a prompt part being rendered and a gate being asked are all
//!   served for one session.
//! * **Waiting.** A tool call can say that its result is not ready yet but will come with a
//!   signal ([`wait_for`]). That is not a failure: whoever ran the call records that it
//!   waits; the run stops before the model sees anything, waits in the log, and takes the
//!   call's result from the signal when it comes. A parent session waits for a child
//!   session this way.
//! * **Stopping.** When a person stops a run, whatever is still working for it can find out
//!   ([`stopping`]) and finish early: a kernel ends the cell it is running, a long tool
//!   returns what it has.
//!
//! With the `std` feature each thread has a scope of its own. Without it there is one for the
//! whole program, which is right for a single-threaded executor and wrong for anything that
//! serves two sessions at once.
//!
//! **Work handed to another thread takes the scope with it.** A thread a tool starts has no
//! scope, so what it does there is done for no session: no stop reaches it, and what it
//! calls is attributed to nobody. [`carry`] takes what this thread serves, and
//! [`Carried::enter`] serves it on another. That such a handle belongs here is a direction
//! and not a decision (B17 in `docs/open-questions.md`); this is a prototype of it, and
//! `docs/prototypes.md` says what it found.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::mem;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::event::SessionId;

/// What a thread is serving.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Scope {
    /// The session being served.
    pub session: SessionId,
    /// The id of the tool call being served, if it is one: not when a prompt part is
    /// rendered or a gate is asked.
    pub call: Option<String>,
}

struct Serving {
    scope: Scope,
    waits: Vec<String>,
    stop: Option<Arc<AtomicBool>>,
    budget: Option<u32>,
}

#[cfg(feature = "std")]
std::thread_local! {
    static SERVING: core::cell::RefCell<Vec<Serving>> =
        const { core::cell::RefCell::new(Vec::new()) };
}

/// Run `work` with this thread's stack of scopes, innermost last.
#[cfg(feature = "std")]
fn serving<R>(work: impl FnOnce(&mut Vec<Serving>) -> R) -> R {
    SERVING.with(|serving| work(&mut serving.borrow_mut()))
}

#[cfg(not(feature = "std"))]
static SERVING: crate::sync::Lock<Vec<Serving>> = crate::sync::Lock::new(Vec::new());

/// Run `work` with the program's stack of scopes, innermost last.
#[cfg(not(feature = "std"))]
fn serving<R>(work: impl FnOnce(&mut Vec<Serving>) -> R) -> R {
    SERVING.with(work)
}

/// A description of what a thread is about to do for a session. Build it, then
/// [`run`](Self::run) the work inside it.
///
/// ```
/// use corduroy_core::SessionId;
/// use corduroy_core::call::{Serve, current};
///
/// let session = SessionId::new("main");
/// let (seen, _waits) = Serve::session(session.clone()).run(|| current().map(|scope| scope.session));
/// assert_eq!(seen, Some(session));
/// assert_eq!(current(), None);
/// ```
#[derive(Debug, Clone)]
pub struct Serve {
    scope: Scope,
    stop: Option<Arc<AtomicBool>>,
    budget: Option<u32>,
}

impl Serve {
    /// Serve `session`, with no tool call: rendering a prompt part, asking a gate.
    #[must_use]
    pub const fn session(session: SessionId) -> Self {
        Self {
            scope: Scope {
                session,
                call: None,
            },
            stop: None,
            budget: None,
        }
    }

    /// Serve the tool call `id` of the session. Only a tool call can [wait](wait_for).
    #[must_use]
    pub fn call(mut self, id: impl Into<String>) -> Self {
        self.scope.call = Some(id.into());
        self
    }

    /// Let the work find out, through [`stopping`], when `flag` is set: the run it is for
    /// was asked to stop.
    #[must_use]
    pub fn stopped_by(mut self, flag: Arc<AtomicBool>) -> Self {
        self.stop = Some(flag);
        self
    }

    /// Let the work find out, through [`budget`], the most its result should take of the
    /// model's context: `tokens`, counted as
    /// [`BYTES_PER_TOKEN`](crate::BYTES_PER_TOKEN) bytes of text to a token.
    #[must_use]
    pub const fn budget(mut self, tokens: u32) -> Self {
        self.budget = Some(tokens);
        self
    }

    /// Run `serve` in this scope. Returns what it returned, and the signals anything on the
    /// call chain said the call waits for, in order.
    ///
    /// Scopes nest: work that serves another call inside this one serves the inner call
    /// until it returns, then this one again.
    pub fn run<R>(self, serve: impl FnOnce() -> R) -> (R, Vec<String>) {
        serving(|serving| {
            serving.push(Serving {
                scope: self.scope,
                waits: Vec::new(),
                stop: self.stop,
                budget: self.budget,
            });
        });
        // Popped even if `serve` panics, so a later call on this thread is not misattributed.
        let guard = Pop;
        let result = serve();
        let waits = serving(|serving| {
            serving
                .last_mut()
                .map(|served| mem::take(&mut served.waits))
        });
        drop(guard);
        (result, waits.unwrap_or_default())
    }
}

/// What a thread was serving when [`carry`] was called, to serve it on another thread.
///
/// A prototype: see [the module](self).
///
/// ```
/// use corduroy_core::SessionId;
/// use corduroy_core::call::{carry, current, within};
///
/// let session = SessionId::new("main");
/// let (seen, _waits) = within(session.clone(), "call-1".into(), || {
///     let carried = carry().expect("a call is being served");
///     // The thread starts with no scope of its own, and serves the call inside `enter`.
///     std::thread::spawn(move || carried.enter(current).0).join().ok().flatten()
/// });
/// assert_eq!(seen.map(|scope| scope.session), Some(session));
/// ```
#[derive(Debug, Clone)]
pub struct Carried {
    /// The scopes that were being served, outermost first.
    serving: Vec<Serve>,
}

/// What this thread is serving, to hand to another thread: the session, the tool call, what
/// stops the run and what bounds the call's result. `None` if it serves nothing.
///
/// The waits are not taken along: see [`Carried::enter`].
#[must_use]
pub fn carry() -> Option<Carried> {
    let carried = serving(|serving| {
        serving
            .iter()
            .map(|served| Serve {
                scope: served.scope.clone(),
                stop: served.stop.clone(),
                budget: served.budget,
            })
            .collect::<Vec<_>>()
    });
    (!carried.is_empty()).then_some(Carried { serving: carried })
}

impl Carried {
    /// Run `work` on this thread as what was [carried](carry) here: it serves the same
    /// session and call, and learns of the same stop. Returns what `work` returned, and the
    /// signals anything it called said the call waits for.
    ///
    /// Those waits belong to the call and reached only this thread. Whoever handed the
    /// work over says them again where the call is served ([`wait_for`], on the thread the
    /// tool was called on), or the run does not wait for them.
    pub fn enter<R>(&self, work: impl FnOnce() -> R) -> (R, Vec<String>) {
        let depth = self.serving.len();
        serving(|serving| {
            serving.extend(self.serving.iter().cloned().map(|serve| Serving {
                scope: serve.scope,
                waits: Vec::new(),
                stop: serve.stop,
                budget: serve.budget,
            }));
        });
        // Popped even if `work` panics, as a scope served here is.
        let guard = PopAll(depth);
        let result = work();
        let waits = serving(|serving| {
            serving
                .last_mut()
                .map(|served| mem::take(&mut served.waits))
        });
        drop(guard);
        (result, waits.unwrap_or_default())
    }
}

/// Pops as many scopes as it holds on drop: when [`Carried::enter`] returns, or its work
/// panics.
struct PopAll(usize);

impl Drop for PopAll {
    fn drop(&mut self) {
        serving(|serving| {
            let kept = serving.len().saturating_sub(self.0);
            serving.truncate(kept);
        });
    }
}

/// Run `serve` as the tool call `call` of `session`. Returns what it returned, and the
/// signals anything on the call chain said the call waits for, in order.
///
/// Short for [`Serve::session`], [`call`](Serve::call) and [`run`](Serve::run).
pub fn within<R>(session: SessionId, call: String, serve: impl FnOnce() -> R) -> (R, Vec<String>) {
    Serve::session(session).call(call).run(serve)
}

/// Pops the innermost scope on drop: when [`Serve::run`] returns, or its work panics.
struct Pop;

impl Drop for Pop {
    fn drop(&mut self) {
        drop(serving(Vec::pop));
    }
}

/// What this thread is serving, if anything.
#[must_use]
pub fn current() -> Option<Scope> {
    serving(|serving| serving.last().map(|served| served.scope.clone()))
}

/// Say that the tool call this thread is serving waits for `signal`: its result will be the
/// signal's payload. Returns `false`, and does nothing, if no tool call is being served.
#[must_use = "a call that cannot wait must answer now"]
pub fn wait_for(signal: &str) -> bool {
    serving(|serving| {
        let Some(served) = serving
            .last_mut()
            .filter(|served| served.scope.call.is_some())
        else {
            return false;
        };
        served.waits.push(signal.to_owned());
        true
    })
}

/// Whether the run this thread works for was asked to stop. Work that can take long should
/// look now and then, and finish early when it is: the result it returns is still recorded.
///
/// `false` when nothing is being served, or whoever serves it cannot be stopped.
#[must_use]
pub fn stopping() -> bool {
    serving(|serving| {
        // A call made for an outer call stops with it.
        serving.iter().any(|served| {
            served
                .stop
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::SeqCst))
        })
    })
}

/// The most the result of the tool call this thread is serving should take, in tokens.
///
/// It is a share of the model's context, counted as
/// [`BYTES_PER_TOKEN`](crate::BYTES_PER_TOKEN) bytes of text to a token. A longer result is
/// cut by whoever runs the call; a tool that asks can choose what to leave out.
///
/// `None` when no tool call is being served, or nothing bounds its result.
#[must_use]
pub fn budget() -> Option<u32> {
    serving(|serving| {
        serving
            .last()
            .filter(|served| served.scope.call.is_some())
            .and_then(|served| served.budget)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_knows_what_its_result_may_take() {
        assert_eq!(budget(), None);
        let serve = Serve::session(SessionId::new("main"))
            .call("1")
            .budget(2048);
        let ((inside, nested), _waits) = serve.run(|| {
            // A call made for this one has a result of its own, and no bound unless given.
            let (nested, _) = within(SessionId::new("main"), "2".into(), budget);
            (budget(), nested)
        });
        assert_eq!(inside, Some(2048));
        assert_eq!(nested, None);
        // A session served without a call has no result to bound.
        let (unserved, _) = Serve::session(SessionId::new("main")).budget(9).run(budget);
        assert_eq!(unserved, None);
    }

    #[test]
    fn a_call_knows_its_session_and_collects_its_waits() {
        assert_eq!(current(), None);
        assert!(!wait_for("nobody"));
        let (inner, waits) = within(SessionId::new("main"), "1".into(), || {
            let outer = current();
            assert!(wait_for("session/main/1/end"));
            let (nested, nested_waits) = within(SessionId::new("main"), "2".into(), || {
                assert!(wait_for("inner"));
                current().and_then(|scope| scope.call)
            });
            assert_eq!(nested_waits, ["inner"]);
            assert_eq!(current(), outer);
            nested
        });
        assert_eq!(inner.as_deref(), Some("2"));
        assert_eq!(waits, ["session/main/1/end"]);
        assert_eq!(current(), None);
    }

    #[test]
    fn a_session_is_served_without_a_call_and_then_cannot_wait() {
        let session = SessionId::new("main/1");
        let ((scope, waited), waits) =
            Serve::session(session.clone()).run(|| (current(), wait_for("anything")));

        assert_eq!(
            scope,
            Some(Scope {
                session,
                call: None
            })
        );
        assert!(!waited);
        assert_eq!(waits, Vec::<String>::new());
    }

    #[test]
    fn work_learns_that_its_run_was_asked_to_stop() {
        assert!(!stopping());
        let flag = Arc::new(AtomicBool::new(false));
        let serve = Serve::session(SessionId::new("main"))
            .call("1")
            .stopped_by(Arc::clone(&flag));

        let ((before, after, nested), _waits) = serve.run(|| {
            let before = stopping();
            flag.store(true, Ordering::SeqCst);
            // A call made for this one, with no flag of its own, stops with it.
            let (nested, _) = within(SessionId::new("main"), "2".into(), stopping);
            (before, stopping(), nested)
        });

        assert_eq!((before, after, nested), (false, true, true));
        assert!(!stopping());
    }

    /// What a thread sees of the scope: what it serves, whether to stop, the result's bound.
    type Seen = (Option<Scope>, bool, Option<u32>);

    fn seen() -> Seen {
        (current(), stopping(), budget())
    }

    /// On a thread of its own: what it sees bare, what it sees inside `carried` (where it
    /// also says the call waits for `ci/done`), and the waits `enter` hands back.
    fn elsewhere(carried: Carried) -> (Seen, Seen, Vec<String>) {
        let work = move || {
            let bare = seen();
            let (inside, waits) = carried.enter(|| (wait_for("ci/done"), seen()).1);
            assert_eq!(current(), None);
            (bare, inside, waits)
        };
        std::thread::spawn(work).join().unwrap()
    }

    #[test]
    fn work_on_another_thread_serves_what_was_carried_to_it() {
        assert!(carry().is_none());
        let flag = Arc::new(AtomicBool::new(false));
        let serve = Serve::session(SessionId::new("main"))
            .call("1")
            .budget(64)
            .stopped_by(Arc::clone(&flag));

        let ((bare, inside, handed_back), said_here) = serve.run(|| {
            let carried = carry().unwrap();
            flag.store(true, Ordering::SeqCst);
            elsewhere(carried)
        });

        // A thread of its own serves nothing until it enters what was carried.
        assert_eq!(bare, (None, false, None));
        let scope = Scope {
            session: SessionId::new("main"),
            call: Some("1".to_owned()),
        };
        assert_eq!(inside, (Some(scope), true, Some(64)));
        // The wait was said on the other thread, and is not this one's until it is said here.
        assert_eq!(handed_back, ["ci/done"]);
        assert_eq!(said_here, Vec::<String>::new());
    }

    #[test]
    fn a_stop_of_an_outer_call_is_carried_with_the_inner_one() {
        let flag = Arc::new(AtomicBool::new(true));
        let outer = Serve::session(SessionId::new("main"))
            .call("1")
            .stopped_by(flag);

        let (carried, _) = outer.run(|| within(SessionId::new("main"), "2".into(), carry).0);
        let (stopped, _) = carried.unwrap().enter(stopping);

        assert!(stopped);
        assert_eq!(current(), None);
    }

    #[test]
    fn a_panicking_call_leaves_no_scope_behind() {
        let panicked = std::panic::catch_unwind(|| {
            within(SessionId::new("main"), "1".into(), || panic!("boom"))
        });
        assert!(panicked.is_err());
        assert_eq!(current(), None);
    }
}
