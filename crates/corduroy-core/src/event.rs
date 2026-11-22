//! The event stream: an append-only log that is also a publish/subscribe bus.
//!
//! Everything that happens in a run is an [`Event`]: a message from the user, an action the
//! agent took, an observation from a tool, a plugin being loaded. Each event records who
//! produced it ([`Source`]) and, where it applies, which earlier event it answers (`cause`).
//! The log is the single source of truth: the conversation shown to a model is a projection of
//! it, and a run can be resumed or replayed from it.
//!
//! One stream holds many *sessions*: conversations, each with its own history. An event that
//! belongs to one names it ([`SessionId`]); events about the harness as a whole, such as a
//! plugin being loaded, name none.
//!
//! Events are added in *commits*: a batch that is stored whole or not at all. A stream may
//! have a [`Log`] behind it, and then a commit is written there before it is added to the
//! stream or shown to any listener, so nothing is ever observed that a restart would not find
//! again. A commit the log refuses closes the stream: every later commit fails with the same
//! error.
//!
//! Delivery is synchronous and in subscription order, so a given log always produces the same
//! sequence of listener calls.

use std::error::Error;
use std::fmt;
use std::str::FromStr;
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

/// Names one session: one conversation among the many a stream can hold.
///
/// The core gives the name no meaning. Whoever starts sessions chooses their ids and records
/// how they relate (a parent, say) in events of its own.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(String);

impl SessionId {
    /// The session called `name`.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for SessionId {
    fn from(name: &str) -> Self {
        Self::new(name)
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

impl FromStr for Source {
    type Err = LogError;

    /// The inverse of `Display`, for stores reading a log back.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "user" => Ok(Self::User),
            "agent" => Ok(Self::Agent),
            "environment" => Ok(Self::Environment),
            "host" => Ok(Self::Host),
            other => other
                .strip_prefix("plugin:")
                .map(|name| Self::Plugin(name.to_owned()))
                .ok_or_else(|| LogError::new(format!("`{other}` is not an event source"))),
        }
    }
}

/// One thing that happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Position in the stream.
    pub id: EventId,
    /// The session the event belongs to, if it belongs to one.
    pub session: Option<SessionId>,
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
        write!(f, "{}", self.id)?;
        if let Some(session) = &self.session {
            write!(f, " {session}")?;
        }
        write!(f, " [{}] {}", self.source, self.topic)?;
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
    session: Option<SessionId>,
    source: Source,
    cause: Option<Cause>,
    topic: String,
    payload: String,
}

impl Draft {
    /// Describe an event from `source` on `topic`, with an empty payload.
    #[must_use]
    pub fn new(source: Source, topic: impl Into<String>) -> Self {
        Self {
            session: None,
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

    /// Record the session the event belongs to.
    #[must_use]
    pub fn session(mut self, session: SessionId) -> Self {
        self.session = Some(session);
        self
    }

    /// Record the event this one answers.
    #[must_use]
    pub const fn cause(mut self, cause: EventId) -> Self {
        self.cause = Some(Cause::Event(cause));
        self
    }

    /// Record that this event answers the draft at `position` (counting from 0) of the batch
    /// it is committed in. The draft named must come earlier in the batch.
    #[must_use]
    pub const fn cause_in_batch(mut self, position: usize) -> Self {
        self.cause = Some(Cause::InBatch(position));
        self
    }
}

impl Draft {
    /// Turn the draft into the next event of `batch`, whose first event follows id `last`.
    fn seal(self, last: u64, batch: &[Event]) -> Result<Event, LogError> {
        let cause = match self.cause {
            None => None,
            Some(Cause::Event(id)) => Some(id),
            Some(Cause::InBatch(position)) => {
                let Some(earlier) = batch.get(position) else {
                    return Err(LogError::new(format!(
                        "draft {} names draft {position} of its batch as its cause",
                        batch.len()
                    )));
                };
                Some(earlier.id)
            }
        };
        Ok(Event {
            id: EventId(last + batch.len() as u64 + 1),
            session: self.session,
            source: self.source,
            cause,
            topic: self.topic,
            payload: self.payload,
        })
    }
}

/// What a [`Draft`] answers: an event already in the stream, or one about to join it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cause {
    Event(EventId),
    InBatch(usize),
}

/// Why a commit could not be stored, or a stored log could not be read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogError(String);

impl LogError {
    /// An error with the given description.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for LogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "event log: {}", self.0)
    }
}

impl Error for LogError {}

/// Durable storage behind an [`EventStream`]. A log has one writer: the stream it was given to.
///
/// The core ships no implementation; a stream without a log keeps its events in memory only.
pub trait Log: Send {
    /// Store `batch` so that it survives a crash, all of it or none of it.
    ///
    /// The stream calls this before it shows the events to anyone. Ids in `batch` continue
    /// directly from the previous commit.
    ///
    /// # Errors
    ///
    /// Returns a [`LogError`] if the batch was not stored. The stream then refuses every
    /// later commit.
    fn commit(&mut self, batch: &[Event]) -> Result<(), LogError>;
}

/// Handle returned by [`EventStream::subscribe`], used to unsubscribe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ListenerId(u64);

type Listener = Arc<dyn Fn(&Event) + Send + Sync>;

/// A committed event and the listeners that should hear about it.
type Delivery = (Event, Vec<Listener>);

struct Subscription {
    id: ListenerId,
    topics: Pattern,
    listener: Listener,
}

#[derive(Default)]
struct Inner {
    log: Vec<Event>,
    store: Option<Box<dyn Log>>,
    /// Set by the first commit the store refused. The stream is closed from then on.
    failed: Option<LogError>,
    subscriptions: Vec<Subscription>,
    next_event: u64,
    next_listener: u64,
}

impl Inner {
    /// Store `drafts` as one batch and pick out the listeners that should hear about each.
    fn commit(&mut self, drafts: Vec<Draft>) -> Result<Vec<Delivery>, LogError> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        let mut batch: Vec<Event> = Vec::with_capacity(drafts.len());
        for draft in drafts {
            let event = draft.seal(self.next_event, &batch)?;
            batch.push(event);
        }
        if let Some(store) = &mut self.store
            && let Err(error) = store.commit(&batch)
        {
            self.failed = Some(error.clone());
            return Err(error);
        }
        self.next_event += batch.len() as u64;
        self.log.extend(batch.iter().cloned());
        Ok(batch
            .into_iter()
            .map(|event| {
                let listeners = self
                    .subscriptions
                    .iter()
                    .filter(|subscription| subscription.topics.matches(&event.topic))
                    .map(|subscription| Arc::clone(&subscription.listener))
                    .collect();
                (event, listeners)
            })
            .collect())
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

    /// Store every future commit in `log` before it is added to the stream.
    ///
    /// Events already in the stream are not written: a log is attached to an empty stream, or
    /// to one [`replay`](Self::replay)ed from that same log.
    #[must_use]
    pub fn with_log(self, log: impl Log + 'static) -> Self {
        self.lock().store = Some(Box::new(log));
        self
    }

    /// Append `drafts` as one commit, then deliver each event to its matching listeners.
    ///
    /// The batch is stored whole before anything else happens. Only then is it added to the
    /// stream and delivered, in order, on the calling thread and in subscription order. A
    /// listener may commit further events; those are stored and delivered before this returns.
    ///
    /// # Errors
    ///
    /// Returns a [`LogError`] if the log refused the batch, now or on an earlier commit, or if
    /// a draft's [`cause_in_batch`](Draft::cause_in_batch) does not name an earlier draft. In
    /// either case nothing was added and no listener was called.
    pub fn commit(
        &self,
        drafts: impl IntoIterator<Item = Draft>,
    ) -> Result<Vec<EventId>, LogError> {
        let committed = self.lock().commit(drafts.into_iter().collect())?;
        // The lock is released, so listeners are free to commit or subscribe.
        Ok(committed
            .into_iter()
            .map(|(event, listeners)| {
                for listener in listeners {
                    listener(&event);
                }
                event.id
            })
            .collect())
    }

    /// Commit a single event. See [`commit`](Self::commit).
    ///
    /// # Errors
    ///
    /// Returns a [`LogError`] if the log refused the event, now or on an earlier commit.
    pub fn emit(&self, draft: Draft) -> Result<EventId, LogError> {
        let ids = self.commit([draft])?;
        ids.first()
            .copied()
            .ok_or_else(|| LogError::new("a commit of one event returned no id"))
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

    /// A copy of the event `id`, if the stream holds it.
    #[must_use]
    pub fn get(&self, id: EventId) -> Option<Event> {
        let inner = self.lock();
        inner
            .log
            .binary_search_by_key(&id, |event| event.id)
            .ok()
            .and_then(|position| inner.log.get(position).cloned())
    }

    /// How many causes lead up to the event `id`: zero for an event nothing caused. Stops
    /// counting at `limit`, so a long chain costs no more than that.
    #[must_use]
    pub fn depth(&self, id: EventId, limit: usize) -> usize {
        let inner = self.lock();
        let mut depth = 0;
        let mut current = id;
        while depth < limit {
            let cause = inner
                .log
                .binary_search_by_key(&current, |event| event.id)
                .ok()
                .and_then(|position| inner.log.get(position))
                .and_then(|event| event.cause);
            let Some(cause) = cause else {
                break;
            };
            depth += 1;
            current = cause;
        }
        drop(inner);
        depth
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

    /// A copy of every event that belongs to `session`, oldest first.
    #[must_use]
    pub fn session(&self, session: &SessionId) -> Vec<Event> {
        self.lock()
            .log
            .iter()
            .filter(|event| event.session.as_ref() == Some(session))
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

        let first = stream.emit(user("message").payload("hi")).unwrap();
        let second = stream
            .emit(Draft::new(Source::Agent, "tool/call").cause(first))
            .unwrap();

        assert_eq!((first.get(), second.get()), (1, 2));
        let events = stream.events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].cause, Some(first));
        assert_eq!(events[0].to_string(), "#1 [user] message hi");
        assert_eq!(events[0].session, None);
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

        stream.emit(user("tool/call")).unwrap();
        stream.emit(user("message")).unwrap();
        stream.emit(user("tool/result")).unwrap();
        stream.unsubscribe(id);
        stream.emit(user("tool/call")).unwrap();

        assert_eq!(seen.load(Ordering::SeqCst), 2);
        assert_eq!(stream.matching(&Pattern::new("tool/*")).len(), 3);
    }

    #[test]
    fn a_listener_may_emit_without_deadlocking() {
        let stream = Arc::new(EventStream::new());
        let inner = Arc::clone(&stream);
        stream.subscribe("ping", move |event| {
            drop(inner.emit(Draft::new(Source::Host, "pong").cause(event.id)));
        });

        stream.emit(user("ping")).unwrap();

        let topics: Vec<String> = stream.events().into_iter().map(|e| e.topic).collect();
        assert_eq!(topics, ["ping", "pong"]);
    }

    #[test]
    fn events_are_found_by_id_and_their_causes_counted() {
        let stream = EventStream::new();
        let first = stream.emit(Draft::new(Source::Host, "a")).unwrap();
        let second = stream
            .emit(Draft::new(Source::Host, "b").cause(first))
            .unwrap();
        let third = stream
            .emit(Draft::new(Source::Host, "c").cause(second))
            .unwrap();

        assert_eq!(
            stream.get(second).map(|event| event.topic),
            Some("b".to_owned())
        );
        assert_eq!(stream.depth(first, 10), 0);
        assert_eq!(stream.depth(third, 10), 2);
        assert_eq!(stream.depth(third, 1), 1);
    }

    #[test]
    fn replay_restores_the_log_and_continues_numbering() {
        let original = EventStream::new();
        original.emit(user("a")).unwrap();
        original.emit(user("b")).unwrap();

        let restored = EventStream::replay(original.events());
        let next = restored.emit(user("c")).unwrap();

        assert_eq!(next.get(), 3);
        assert_eq!(restored.len(), 3);
        assert!(!restored.is_empty());
        assert_eq!(Source::Plugin("skills".into()).to_string(), "plugin:skills");
    }

    /// A log that keeps its batches where the test can see them and can be told to fail.
    #[derive(Default)]
    struct Recorder {
        batches: Arc<Mutex<Vec<Vec<Event>>>>,
        broken: Arc<AtomicUsize>,
    }

    impl Log for Recorder {
        fn commit(&mut self, batch: &[Event]) -> Result<(), LogError> {
            if self.broken.load(Ordering::SeqCst) > 0 {
                return Err(LogError::new("disk full"));
            }
            self.batches
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(batch.to_vec());
            Ok(())
        }
    }

    #[test]
    fn a_commit_is_stored_whole_before_any_listener_hears_of_it() {
        let recorder = Recorder::default();
        let batches = Arc::clone(&recorder.batches);
        let stream = EventStream::new().with_log(recorder);
        let stored_when_heard = Arc::new(Mutex::new(Vec::new()));
        let (seen, stored) = (Arc::clone(&stored_when_heard), Arc::clone(&batches));
        stream.subscribe("*", move |_| {
            seen.lock().unwrap().push(stored.lock().unwrap().len());
        });

        let ids = stream
            .commit([
                user("message"),
                Draft::new(Source::Agent, "tool/call").cause_in_batch(0),
            ])
            .unwrap();

        assert_eq!(ids.len(), 2);
        assert_eq!(stream.events()[1].cause, Some(ids[0]));
        assert_eq!(batches.lock().unwrap().as_slice(), [stream.events()]);
        // Both listener calls happened after the one batch was in the log.
        assert_eq!(*stored_when_heard.lock().unwrap(), [1, 1]);
    }

    #[test]
    fn a_refused_commit_adds_nothing_and_closes_the_stream() {
        let recorder = Recorder::default();
        let broken = Arc::clone(&recorder.broken);
        let stream = EventStream::new().with_log(recorder);
        let heard = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&heard);
        stream.subscribe("*", move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        stream.emit(user("kept")).unwrap();

        broken.store(1, Ordering::SeqCst);
        let error = stream.commit([user("a"), user("b")]).unwrap_err();
        broken.store(0, Ordering::SeqCst);

        assert_eq!(error.to_string(), "event log: disk full");
        assert_eq!((stream.len(), heard.load(Ordering::SeqCst)), (1, 1));
        // The log may hold part of the batch, so the stream does not carry on past it.
        assert_eq!(stream.emit(user("later")), Err(error));
    }

    #[test]
    fn a_cause_must_be_an_earlier_draft_of_the_batch() {
        let stream = EventStream::new();

        let error = stream.commit([user("a").cause_in_batch(0)]).unwrap_err();

        assert!(
            error.to_string().contains("draft 0 names draft 0"),
            "{error}"
        );
        assert!(stream.is_empty());
        // A bad batch is the caller's mistake, not the log's: the stream stays open.
        assert!(stream.emit(user("b")).is_ok());
    }

    #[test]
    fn events_name_their_session_and_can_be_read_by_it() {
        let stream = EventStream::new();
        let (main, child) = (SessionId::new("main"), SessionId::from("child"));

        stream.emit(user("message").session(main.clone())).unwrap();
        stream.emit(user("message").session(child.clone())).unwrap();
        stream
            .emit(Draft::new(Source::Host, "plugin/activated"))
            .unwrap();
        stream.emit(user("message").session(main.clone())).unwrap();

        let ids = |events: Vec<Event>| events.iter().map(|e| e.id.get()).collect::<Vec<_>>();
        assert_eq!(ids(stream.session(&main)), [1, 4]);
        assert_eq!(ids(stream.session(&child)), [2]);
        assert_eq!(stream.events()[0].to_string(), "#1 main [user] message");
        assert_eq!(child.as_str(), "child");
    }

    #[test]
    fn sources_parse_back_from_their_display_form() {
        for source in [
            Source::User,
            Source::Agent,
            Source::Environment,
            Source::Host,
            Source::Plugin("skills".into()),
        ] {
            assert_eq!(source.to_string().parse(), Ok(source));
        }
        assert!("nobody".parse::<Source>().is_err());
    }
}
