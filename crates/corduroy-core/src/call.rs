//! The tool call being served on this thread, and the signals it waits for.
//!
//! A tool runs synchronously on the thread that calls it, and so does everything it calls in
//! turn: a WebAssembly plugin's host functions, a service another plugin provides. Whoever
//! runs a tool on behalf of a session wraps the call in [`within`], and anything on that call
//! chain can then ask which session and which call it serves ([`current`]), so that events it
//! raises carry the session, and can say that the call's result is not ready yet but will come
//! with a signal ([`wait_for`]).
//!
//! A call that waits is not a failure. Whoever ran it records that it waits; the run stops
//! before the model sees anything, waits in the log, and takes the call's result from the
//! signal when it comes. That is how a parent session waits for a child session: the call
//! that started the child waits for the signal its end raises.

use std::cell::RefCell;

use crate::event::SessionId;

/// The call a thread is serving.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Scope {
    /// The session the call belongs to.
    pub session: SessionId,
    /// The tool call's id.
    pub call: String,
}

struct Serving {
    scope: Scope,
    waits: Vec<String>,
}

thread_local! {
    static SERVING: RefCell<Vec<Serving>> = const { RefCell::new(Vec::new()) };
}

/// Run `serve` as the tool call `call` of `session`. Returns what it returned, and the
/// signals anything on the call chain said the call waits for, in order.
///
/// Calls nest: a tool that calls another tool through [`within`] serves the inner call
/// until it returns, then the outer one again.
pub fn within<R>(session: SessionId, call: String, serve: impl FnOnce() -> R) -> (R, Vec<String>) {
    SERVING.with(|serving| {
        serving.borrow_mut().push(Serving {
            scope: Scope { session, call },
            waits: Vec::new(),
        });
    });
    // Popped even if `serve` panics, so a later call on this thread is not misattributed.
    let guard = Pop;
    let result = serve();
    drop(guard);
    let waits = SERVING.with(|serving| serving.borrow_mut().pop().map(|served| served.waits));
    (result, waits.unwrap_or_default())
}

/// Pops the innermost call on drop, unless [`within`] already did.
struct Pop;

impl Drop for Pop {
    fn drop(&mut self) {
        if std::thread::panicking() {
            SERVING.with(|serving| drop(serving.borrow_mut().pop()));
        }
    }
}

/// The call this thread is serving, if any.
#[must_use]
pub fn current() -> Option<Scope> {
    SERVING.with(|serving| serving.borrow().last().map(|served| served.scope.clone()))
}

/// Say that the call this thread is serving waits for `signal`: its result will be the
/// signal's payload. Returns `false`, and does nothing, if no call is being served.
#[must_use = "a call that cannot wait must answer now"]
pub fn wait_for(signal: &str) -> bool {
    SERVING.with(|serving| {
        let mut serving = serving.borrow_mut();
        let Some(served) = serving.last_mut() else {
            return false;
        };
        served.waits.push(signal.to_owned());
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_knows_its_session_and_collects_its_waits() {
        assert_eq!(current(), None);
        assert!(!wait_for("nobody"));
        let (inner, waits) = within(SessionId::new("main"), "1".into(), || {
            let outer = current();
            assert!(wait_for("session/main/1/end"));
            let (nested, nested_waits) = within(SessionId::new("main"), "2".into(), || {
                assert!(wait_for("inner"));
                current().map(|scope| scope.call)
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
    fn a_panicking_call_leaves_no_scope_behind() {
        let panicked = std::panic::catch_unwind(|| {
            within(SessionId::new("main"), "1".into(), || panic!("boom"))
        });
        assert!(panicked.is_err());
        assert_eq!(current(), None);
    }
}
