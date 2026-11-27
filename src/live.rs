//! What a model is writing, shown while it writes: the live channel.
//!
//! A reply is committed whole, as one `message` event, when the model has finished it. A
//! person watching would rather read it as it comes. So each [`Piece`] a provider tells the
//! harness ([`Reports::piece`](crate::provider::Reports::piece)) is handed to whoever
//! [watches](Live::watch) the session, and is not an event.
//!
//! It is a preview and nothing else. Nothing is rebuilt from it, nothing waits for it and
//! nothing may depend on having seen all of it: a watcher that reads slowly finds older
//! pieces gone ([`Seen::Missed`]) and the run none the slower. What a reply said is in the
//! log once it is whole, and where it never became whole, in its checkpoints
//! (`model/progress`) and its `model/partial`.
//!
//! A [`Watch`] is read from the watcher's own task or thread, never from the run's: telling
//! a piece only puts it in the watcher's queue and wakes whoever waits there.

use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::future::poll_fn;
use core::mem;
use core::task::{Poll, Waker};
#[cfg(feature = "std")]
use core::time::Duration;
#[cfg(feature = "std")]
use std::task::Wake;
#[cfg(feature = "std")]
use std::thread::{self, Thread};

use corduroy_core::SessionId;
use corduroy_core::sync::Lock;

use crate::provider::Piece;

/// The most text one watcher is kept, in bytes, before the oldest of it is let go.
pub const WATCH_BYTES: usize = 64 * 1024;

/// The most entries one watcher is kept. Text that follows text is one entry, so this is
/// only reached by pieces that do not join.
pub const WATCH_ENTRIES: usize = 1024;

/// Something a watcher is shown about the request a session's model is answering.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Seen {
    /// The model wrote this. Text that followed text, and thinking that followed thinking,
    /// may have been joined into one piece on the way.
    Piece(Piece),
    /// The attempt failed and the request is being made again: what was shown of its answer
    /// is void, and the next pieces start a new one.
    Restarted,
    /// The watcher read too slowly and pieces before this were let go. What follows is not
    /// the whole of the answer; the committed message is.
    Missed,
}

/// One thing shown, and the session whose request it is of.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Shown {
    /// The session whose model is writing.
    pub session: SessionId,
    /// What it wrote, or what became of it.
    pub seen: Seen,
}

/// What one watcher has not read yet.
#[derive(Default)]
struct Unread {
    shown: VecDeque<Shown>,
    /// The bytes of text in `shown`.
    bytes: usize,
    /// Sessions of which something was let go, to be told before anything else.
    missed: Vec<SessionId>,
    /// Whoever waits for more.
    waiting: Option<Waker>,
}

/// One watcher's queue.
struct Queue {
    /// The session watched, or every session.
    session: Option<SessionId>,
    unread: Lock<Unread>,
}

impl Queue {
    /// Everything unread, or `waker` left to be woken when there is something.
    fn poll_take(&self, waker: &Waker) -> Poll<Vec<Shown>> {
        self.unread.with(|unread| {
            if unread.is_empty() {
                unread.waiting = Some(waker.clone());
                Poll::Pending
            } else {
                Poll::Ready(unread.take())
            }
        })
    }
}

/// How many bytes of text `piece` holds.
const fn size(piece: &Piece) -> usize {
    match piece {
        Piece::Text(text) | Piece::Thinking(text) => text.len(),
        Piece::Call(call) => call.name.len().saturating_add(call.arguments.len()),
        _ => 0,
    }
}

/// Add `piece` to the end of `last` if both are text, or both thinking. Whether it was.
fn joined(last: &mut Seen, piece: &Piece) -> bool {
    match (last, piece) {
        (Seen::Piece(Piece::Text(text)), Piece::Text(more))
        | (Seen::Piece(Piece::Thinking(text)), Piece::Thinking(more)) => {
            text.push_str(more);
            true
        }
        _ => false,
    }
}

impl Unread {
    /// Take in one more thing shown, joining text to text, and let the oldest go where
    /// there is now too much.
    fn take_in(&mut self, session: &SessionId, seen: &Seen) {
        if let Seen::Piece(piece) = seen {
            self.bytes = self.bytes.saturating_add(size(piece));
            let joins = self
                .shown
                .back_mut()
                .is_some_and(|last| last.session == *session && joined(&mut last.seen, piece));
            if joins {
                self.trim();
                return;
            }
        }
        self.shown.push_back(Shown {
            session: session.clone(),
            seen: seen.clone(),
        });
        self.trim();
    }

    /// Let the oldest entries go until what is kept is within bounds, noting whose they
    /// were. The newest is kept whatever its size, cut to the bound from its end.
    fn trim(&mut self) {
        while self.shown.len() > 1 && (self.bytes > WATCH_BYTES || self.shown.len() > WATCH_ENTRIES)
        {
            let Some(gone) = self.shown.pop_front() else {
                return;
            };
            if let Seen::Piece(piece) = &gone.seen {
                self.bytes = self.bytes.saturating_sub(size(piece));
            }
            self.miss(gone.session);
        }
        if self.bytes <= WATCH_BYTES {
            return;
        }
        // One piece that is itself too long: its end is kept.
        let Some(Shown { session, seen }) = self.shown.back_mut() else {
            return;
        };
        if let Seen::Piece(Piece::Text(text) | Piece::Thinking(text)) = seen {
            let mut from = text.len().saturating_sub(WATCH_BYTES);
            while !text.is_char_boundary(from) {
                from = from.saturating_add(1);
            }
            drop(text.drain(..from));
            self.bytes = text.len();
            let session = session.clone();
            self.miss(session);
        }
    }

    /// Note that something of `session` was let go.
    fn miss(&mut self, session: SessionId) {
        if !self.missed.contains(&session) {
            self.missed.push(session);
        }
    }

    /// Everything unread, oldest first, with what was missed said before it.
    fn take(&mut self) -> Vec<Shown> {
        let missed = mem::take(&mut self.missed)
            .into_iter()
            .map(|session| Shown {
                session,
                seen: Seen::Missed,
            });
        self.bytes = 0;
        missed.chain(mem::take(&mut self.shown)).collect()
    }

    fn is_empty(&self) -> bool {
        self.shown.is_empty() && self.missed.is_empty()
    }
}

/// The live channel: what each session's model is writing, for whoever watches. See the
/// [module](self).
#[derive(Default)]
pub struct Live {
    watchers: Lock<Vec<Weak<Queue>>>,
}

impl core::fmt::Debug for Live {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Live").finish_non_exhaustive()
    }
}

impl Live {
    /// Watch what the model of `session` writes, or of every session where none is named.
    /// The watch ends when it is dropped.
    #[must_use]
    pub fn watch(&self, session: Option<&SessionId>) -> Watch {
        let queue = Arc::new(Queue {
            session: session.cloned(),
            unread: Lock::default(),
        });
        self.watchers
            .with(|watchers| watchers.push(Arc::downgrade(&queue)));
        Watch { queue }
    }

    /// Show `seen` of `session` to everyone watching it. Never waits: a watcher that has
    /// not read what it was shown loses the oldest of it.
    pub(crate) fn tell(&self, session: &SessionId, seen: &Seen) {
        let watching: Vec<Arc<Queue>> = self.watchers.with(|watchers| {
            watchers.retain(|watcher| watcher.strong_count() > 0);
            watchers.iter().filter_map(Weak::upgrade).collect()
        });
        for queue in watching {
            if queue
                .session
                .as_ref()
                .is_some_and(|watched| watched != session)
            {
                continue;
            }
            let waiting = queue.unread.with(|unread| {
                unread.take_in(session, seen);
                unread.waiting.take()
            });
            // Woken outside the lock: a waker may run code that reads the watch.
            if let Some(waker) = waiting {
                waker.wake();
            }
        }
    }
}

/// One watcher's end of the [live channel](Live): what it has been shown and not yet read.
pub struct Watch {
    queue: Arc<Queue>,
}

impl core::fmt::Debug for Watch {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Watch")
            .field("session", &self.queue.session)
            .finish_non_exhaustive()
    }
}

impl Watch {
    /// Everything shown since the last read, oldest first; nothing, if there was nothing.
    #[must_use]
    pub fn take(&self) -> Vec<Shown> {
        self.queue.unread.with(Unread::take)
    }

    /// Everything shown since the last read, waiting until there is something. On any
    /// executor: whoever waits leaves its waker.
    pub async fn next(&self) -> Vec<Shown> {
        poll_fn(|cx| self.queue.poll_take(cx.waker())).await
    }

    /// Everything shown since the last read, waiting on this thread for at most `at_most`
    /// until there is something. Nothing, if nothing came in that time.
    #[cfg(feature = "std")]
    #[must_use]
    pub fn wait(&self, at_most: Duration) -> Vec<Shown> {
        let waker = Waker::from(Arc::new(Unpark(thread::current())));
        let shown = self.queue.unread.with(|unread| {
            if unread.is_empty() {
                unread.waiting = Some(waker);
            }
            unread.take()
        });
        if !shown.is_empty() {
            return shown;
        }
        thread::park_timeout(at_most);
        self.take()
    }
}

/// Wakes a thread parked in [`Watch::wait`].
#[cfg(feature = "std")]
struct Unpark(Thread);

#[cfg(feature = "std")]
impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

#[cfg(test)]
mod tests {
    use alloc::borrow::ToOwned;
    use alloc::string::String;

    use super::*;
    use crate::provider::CallPiece;

    fn text(text: &str) -> Seen {
        Seen::Piece(Piece::Text(text.to_owned()))
    }

    #[test]
    fn a_watcher_is_shown_its_session_in_order_and_text_is_joined() {
        let live = Live::default();
        let (main, other) = (SessionId::new("main"), SessionId::new("other"));
        let one = live.watch(Some(&main));
        let all = live.watch(None);

        live.tell(&main, &text("Hel"));
        live.tell(&main, &text("lo"));
        live.tell(&other, &text("elsewhere"));
        let call = Seen::Piece(Piece::Call(CallPiece::new("1", "read", "{}")));
        live.tell(&main, &call);
        live.tell(&main, &Seen::Restarted);

        let seen: Vec<Seen> = one.take().into_iter().map(|shown| shown.seen).collect();
        assert_eq!(seen, [text("Hello"), call, Seen::Restarted]);
        assert_eq!(one.take(), []);
        assert_eq!(all.take().len(), 4);
    }

    #[test]
    fn a_watcher_that_does_not_read_loses_the_oldest_and_is_told() {
        let live = Live::default();
        let main = SessionId::new("main");
        let watch = live.watch(Some(&main));
        let long = "x".repeat(WATCH_BYTES);

        live.tell(&main, &text("first"));
        live.tell(&main, &Seen::Restarted);
        live.tell(&main, &text(&long));
        live.tell(&main, &text("é"));

        let shown = watch.take();
        assert_eq!(shown.first().map(|shown| &shown.seen), Some(&Seen::Missed));
        let kept: usize = shown
            .iter()
            .map(|shown| match &shown.seen {
                Seen::Piece(piece) => size(piece),
                _ => 0,
            })
            .sum();
        assert!(kept <= WATCH_BYTES, "{kept}");
        let Some(Seen::Piece(Piece::Text(last))) = shown.last().map(|shown| &shown.seen) else {
            panic!("{shown:?}");
        };
        assert!(last.ends_with('é'));
    }

    #[test]
    fn a_dropped_watch_is_forgotten() {
        let live = Live::default();
        let main = SessionId::new("main");
        drop(live.watch(None));
        live.tell(&main, &text("nobody"));
        assert!(live.watchers.with(|watchers| watchers.is_empty()));
    }

    #[cfg(feature = "std")]
    #[test]
    fn waiting_on_a_thread_wakes_when_something_is_shown() {
        let live = Arc::new(Live::default());
        let main = SessionId::new("main");
        let watch = live.watch(None);
        assert_eq!(watch.wait(Duration::from_millis(1)), []);

        let teller = Arc::clone(&live);
        let telling = thread::spawn(move || teller.tell(&main, &text("now")));
        let mut shown = Vec::new();
        for _ in 0..200 {
            shown = watch.wait(Duration::from_millis(50));
            if !shown.is_empty() {
                break;
            }
        }
        telling.join().unwrap();
        let said: String = shown
            .iter()
            .filter_map(|shown| match &shown.seen {
                Seen::Piece(Piece::Text(text)) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(said, "now");
    }

    #[tokio::test]
    async fn waiting_in_a_task_wakes_when_something_is_shown() {
        let live = Arc::new(Live::default());
        let main = SessionId::new("main");
        let watch = live.watch(Some(&main));
        let teller = Arc::clone(&live);
        let waiting = tokio::spawn(async move { watch.next().await });
        tokio::task::yield_now().await;

        teller.tell(&main, &text("there"));

        assert_eq!(waiting.await.unwrap().len(), 1);
    }
}
