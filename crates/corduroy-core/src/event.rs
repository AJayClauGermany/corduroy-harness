//! The event stream: an append-only log that is also a publish/subscribe bus.
//!
//! Everything that happens in a run is an [`Event`]: a message from the user, an action the
//! agent took, an observation from a tool, a plugin being loaded. Each event records who
//! produced it ([`Source`]) and, where it applies, which earlier event it answers (`cause`).
//! The log is the single source of truth: the conversation shown to a model is a projection of
//! it, and a run can be resumed or replayed from it.
//!
//! Delivery is synchronous and in subscription order, so a given log always produces the same
//! sequence of listener calls.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::pattern::Pattern;

/// Position of an event in its stream. Ids are assigned in order, starting at 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventId(u64);

impl EventId {
    /// Wrap a raw id, for example one read back from a persisted log.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw id.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Who produced an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The person or program driving the agent.
    User,
    /// The model.
    Agent,
    /// A tool or kernel reporting what happened.
    Environment,
    /// The named plugin.
    Plugin(String),
    /// The harness itself.
    Host,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => f.write_str("user"),
            Self::Agent => f.write_str("agent"),
            Self::Environment => f.write_str("environment"),
            Self::Plugin(name) => write!(f, "plugin:{name}"),
            Self::Host => f.write_str("host"),
        }
    }
}

/// One thing that happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Position in the stream.
    pub id: EventId,
    /// Who produced the event.
    pub source: Source,
    /// The earlier event this one answers, if any.
    pub cause: Option<EventId>,
    /// What kind of event this is, as a `/`-separated path such as `tool/result`.
    pub topic: String,
    /// The event body. By convention JSON, so it can cross the plugin boundary unchanged.
    pub payload: String,
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}] {}", self.id, self.source, self.topic)?;
        if let Some(cause) = self.cause {
            write!(f, " (after {cause})")?;
        }
        if !self.payload.is_empty() {
            write!(f, " {}", self.payload)?;
        }
        Ok(())
    }
}

/// An event that has not been added to a stream yet, so has no id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    source: Source,
    cause: Option<EventId>,
    topic: String,
    payload: String,
}

impl Draft {
    /// Describe an event from `source` on `topic`, with an empty payload.
    #[must_use]
    pub fn new(source: Source, topic: impl Into<String>) -> Self {
        Self {
            source,
            cause: None,
            topic: topic.into(),
            payload: String::new(),
        }
    }

    /// Set the payload.
    #[must_use]
    pub fn payload(mut self, payload: impl Into<String>) -> Self {
        self.payload = payload.into();
        self
    }

    /// Record the event this one answers.
    #[must_use]
    pub const fn cause(mut self, cause: EventId) -> Self {
        self.cause = Some(cause);
        self
    }
}

/// Handle returned by [`EventStream::subscribe`], used to unsubscribe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ListenerId(u64);

type Listener = Arc<dyn Fn(&Event) + Send + Sync>;

struct Subscription {
    id: ListenerId,
    topics: Pattern,
    listener: Listener,
}

#[derive(Default)]
struct Inner {
    log: Vec<Event>,
    subscriptions: Vec<Subscription>,
    next_event: u64,
    next_listener: u64,
}

impl Inner {
    /// Log `draft` and pick out the listeners that should hear about it.
    fn append(&mut self, draft: Draft) -> (Event, Vec<Listener>) {
        self.next_event += 1;
        let event = Event {
            id: EventId(self.next_event),
            source: draft.source,
            cause: draft.cause,
            topic: draft.topic,
            payload: draft.payload,
        };
        self.log.push(event.clone());
        let listeners = self
            .subscriptions
            .iter()
            .filter(|subscription| subscription.topics.matches(&event.topic))
            .map(|subscription| Arc::clone(&subscription.listener))
            .collect();
        (event, listeners)
    }
}

/// An append-only log of [`Event`]s that notifies listeners as events arrive.
#[derive(Default)]
pub struct EventStream {
    inner: Mutex<Inner>,
}

impl fmt::Debug for EventStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.lock();
        f.debug_struct("EventStream")
            .field("events", &inner.log.len())
            .field("listeners", &inner.subscriptions.len())
            .finish()
    }
}

impl EventStream {
    /// An empty stream.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild a stream from a persisted log. Listeners are not notified of replayed events.
    ///
    /// New events continue after the highest id in `events`.
    #[must_use]
    pub fn replay(events: impl IntoIterator<Item = Event>) -> Self {
        let log: Vec<Event> = events.into_iter().collect();
        let next_event = log.iter().map(|event| event.id.get()).max().unwrap_or(0);
        Self {
            inner: Mutex::new(Inner {
                log,
                next_event,
                ..Inner::default()
            }),
        }
    }

    /// Append an event and deliver it to every matching listener before returning.
    ///
    /// Listeners run on the calling thread in subscription order. A listener may emit further
    /// events; those are appended and delivered before `emit` returns.
    pub fn emit(&self, draft: Draft) -> EventId {
        let (event, listeners) = self.lock().append(draft);
        // The lock is released, so listeners are free to emit or subscribe.
        for listener in listeners {
            listener(&event);
        }
        event.id
    }

    /// Call `listener` for every future event whose topic matches `topics`.
    pub fn subscribe(
        &self,
        topics: impl Into<Pattern>,
        listener: impl Fn(&Event) + Send + Sync + 'static,
    ) -> ListenerId {
        let mut inner = self.lock();
        inner.next_listener += 1;
        let id = ListenerId(inner.next_listener);
        inner.subscriptions.push(Subscription {
            id,
            topics: topics.into(),
            listener: Arc::new(listener),
        });
        id
    }

    /// Stop delivering to a listener. Unknown ids are ignored.
    pub fn unsubscribe(&self, id: ListenerId) {
        self.lock()
            .subscriptions
            .retain(|subscription| subscription.id != id);
    }

    /// A copy of every event so far, oldest first.
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        self.lock().log.clone()
    }

    /// A copy of every event whose topic matches `topics`, oldest first.
    #[must_use]
    pub fn matching(&self, topics: &Pattern) -> Vec<Event> {
        self.lock()
            .log
            .iter()
            .filter(|event| topics.matches(&event.topic))
            .cloned()
            .collect()
    }

    /// How many events have been emitted.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().log.len()
    }

    /// Whether nothing has been emitted yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A listener that panicked cannot leave the log half-written: every mutation above
        // completes before listeners run. Recover the guard rather than propagate the poison.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn user(topic: &str) -> Draft {
        Draft::new(Source::User, topic)
    }

    #[test]
    fn events_are_logged_in_order_with_sequential_ids() {
        let stream = EventStream::new();

        let first = stream.emit(user("message").payload("hi"));
        let second = stream.emit(Draft::new(Source::Agent, "tool/call").cause(first));

        assert_eq!((first.get(), second.get()), (1, 2));
        let events = stream.events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].cause, Some(first));
        assert_eq!(events[0].to_string(), "#1 [user] message hi");
        assert_eq!(events[1].to_string(), "#2 [agent] tool/call (after #1)");
    }

    #[test]
    fn listeners_see_only_matching_topics_until_unsubscribed() {
        let stream = EventStream::new();
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        let id = stream.subscribe("tool/*", move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        });

        stream.emit(user("tool/call"));
        stream.emit(user("message"));
        stream.emit(user("tool/result"));
        stream.unsubscribe(id);
        stream.emit(user("tool/call"));

        assert_eq!(seen.load(Ordering::SeqCst), 2);
        assert_eq!(stream.matching(&Pattern::new("tool/*")).len(), 3);
    }

    #[test]
    fn a_listener_may_emit_without_deadlocking() {
        let stream = Arc::new(EventStream::new());
        let inner = Arc::clone(&stream);
        stream.subscribe("ping", move |event| {
            inner.emit(Draft::new(Source::Host, "pong").cause(event.id));
        });

        stream.emit(user("ping"));

        let topics: Vec<String> = stream.events().into_iter().map(|e| e.topic).collect();
        assert_eq!(topics, ["ping", "pong"]);
    }

    #[test]
    fn replay_restores_the_log_and_continues_numbering() {
        let original = EventStream::new();
        original.emit(user("a"));
        original.emit(user("b"));

        let restored = EventStream::replay(original.events());
        let next = restored.emit(user("c"));

        assert_eq!(next.get(), 3);
        assert_eq!(restored.len(), 3);
        assert!(!restored.is_empty());
        assert_eq!(Source::Plugin("skills".into()).to_string(), "plugin:skills");
    }
}
