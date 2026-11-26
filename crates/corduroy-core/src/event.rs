//! The event stream: an append-only log that is also a publish/subscribe bus.
//!
//! Everything that happens in a run is an [`Event`]: a message from the user, an action the
//! agent took, an observation from a tool, a plugin being loaded. Each event records who
//! produced it ([`Source`]) and, where it applies, which earlier event it answers (`cause`).
//! The log is the single source of truth: the conversation shown to a model is a projection of
//! it, and a run can be read back and resumed from it.
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
//! Every event carries the time its commit was stored, in milliseconds since the Unix epoch.
//! The stream reads the clock once per commit, so the events of one commit share a time, and
//! never lets time run backwards: a log's times are in order, like its ids.
//!
//! Delivery is synchronous, on the thread that committed, and in subscription order. Commits
//! made on one thread are therefore delivered in the order they were made. Two threads that
//! commit at the same moment each deliver their own commit, so a listener may hear the later
//! of the two first; what a listener must not get wrong it reads from the log, by id.
//!
//! A read promises a complete answer about what the log holds, not that the stream holds it
//! in memory. The stream holds what its store handed it when it was opened (the whole log,
//! unless the store says otherwise: see [`Log::read`]) and every commit since.
//! [`EventStream::read`] reads a page, and asks the store for whatever the stream does not
//! hold, so it is complete in every case and can fail. The reads that cannot fail
//! ([`events`](EventStream::events), [`session`](EventStream::session),
//! [`matching`](EventStream::matching), [`after`](EventStream::after),
//! [`get`](EventStream::get)) are the convenient form: they answer from what the stream holds.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::error::Error;
use core::fmt;
use core::str::FromStr;

use crate::sync::Lock;

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
    /// When the event's commit was stored, in milliseconds since the Unix epoch. Events of
    /// one commit share a time, and times never decrease along a stream.
    pub time: u64,
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
    /// Turn the draft into the next event of `batch`, whose first event follows id `last`
    /// and which is stored at `time`.
    fn seal(self, last: u64, time: u64, batch: &[Event]) -> Result<Event, LogError> {
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
            time,
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

    /// Read back what the stream may not hold: the first `query.limit` stored events that
    /// match `query` and `keep`, oldest first.
    ///
    /// A store that hands the stream every event it holds when it is opened need not answer:
    /// by default it finds nothing, and [`EventStream::read`] answers from the stream. A
    /// store that hands over only part of the log must answer for at least every event it
    /// did not hand over. It may also return events the stream holds; the stream keeps one
    /// of each.
    ///
    /// # Errors
    ///
    /// Returns a [`LogError`] if the store could not be read. Unlike a refused commit, this
    /// leaves the stream open.
    fn read(
        &mut self,
        _query: &Query,
        _keep: &dyn Fn(&Event) -> bool,
    ) -> Result<Vec<Event>, LogError> {
        Ok(Vec::new())
    }
}

/// Which events a page of [`EventStream::read`] holds: those after an id, oldest first, at
/// most so many, of one session and on some topics if either is given.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Query {
    /// Only events with an id above this one. The next page starts after the last event
    /// of this one.
    pub after: EventId,
    /// At most this many events.
    pub limit: usize,
    /// Only the events of this session, if one is given.
    pub session: Option<SessionId>,
    /// Only events whose topic matches this, if it is given.
    pub topics: Option<Pattern>,
}

impl Query {
    /// The first `limit` events of the log.
    #[must_use]
    pub const fn first(limit: usize) -> Self {
        Self {
            after: EventId(0),
            limit,
            session: None,
            topics: None,
        }
    }

    /// Only events with an id above `id`.
    #[must_use]
    pub const fn after(mut self, id: EventId) -> Self {
        self.after = id;
        self
    }

    /// Only the events of `session`.
    #[must_use]
    pub fn session(mut self, session: SessionId) -> Self {
        self.session = Some(session);
        self
    }

    /// Only events whose topic matches `topics`.
    #[must_use]
    pub fn topics(mut self, topics: impl Into<Pattern>) -> Self {
        self.topics = Some(topics.into());
        self
    }

    /// Whether `event` is one this query asks for, but for its id and the limit.
    #[must_use]
    pub fn selects(&self, event: &Event) -> bool {
        self.session
            .as_ref()
            .is_none_or(|session| event.session.as_ref() == Some(session))
            && self
                .topics
                .as_ref()
                .is_none_or(|topics| topics.matches(&event.topic))
    }
}

/// Handle returned by [`EventStream::subscribe`], used to unsubscribe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ListenerId(u64);

type Listener = Arc<dyn Fn(&Event) + Send + Sync>;

/// Where a stream reads the time from: milliseconds since the Unix epoch.
type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The system clock, in milliseconds since the Unix epoch.
#[cfg(feature = "std")]
fn system_clock() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Without the standard library there is no system clock: the epoch, until a clock is given
/// with [`EventStream::with_clock`]. Commits still never go back in time.
#[cfg(not(feature = "std"))]
const fn system_clock() -> u64 {
    0
}

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
    /// The time of the last commit, below which no later commit is stamped.
    last_time: u64,
    /// Read instead of the system clock, if set.
    clock: Option<Clock>,
}

impl Inner {
    /// Store `drafts` as one batch and pick out the listeners that should hear about each.
    fn commit(&mut self, drafts: Vec<Draft>) -> Result<Vec<Delivery>, LogError> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        // A commit of nothing is nothing: no line in the log, and no wait for a disk.
        if drafts.is_empty() {
            return Ok(Vec::new());
        }
        // One reading per commit, and never an earlier one than the commit before had.
        let now = self
            .clock
            .as_ref()
            .map_or_else(system_clock, |clock| clock());
        let time = now.max(self.last_time);
        let mut batch: Vec<Event> = Vec::with_capacity(drafts.len());
        for draft in drafts {
            let event = draft.seal(self.next_event, time, &batch)?;
            batch.push(event);
        }
        if let Some(store) = &mut self.store
            && let Err(error) = store.commit(&batch)
        {
            self.failed = Some(error.clone());
            return Err(error);
        }
        self.next_event += batch.len() as u64;
        self.last_time = time;
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
    inner: Lock<Inner>,
}

impl fmt::Debug for EventStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (events, listeners) = self
            .inner
            .with(|inner| (inner.log.len(), inner.subscriptions.len()));
        f.debug_struct("EventStream")
            .field("events", &events)
            .field("listeners", &listeners)
            .finish()
    }
}

impl EventStream {
    /// An empty stream.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A stream holding `events`, read back from a log. Nothing is re-executed: listeners
    /// are not told of these events, which happened before they subscribed.
    ///
    /// New events continue after the highest id in `events`.
    #[must_use]
    pub fn load(events: impl IntoIterator<Item = Event>) -> Self {
        let log: Vec<Event> = events.into_iter().collect();
        let next_event = log.iter().map(|event| event.id.get()).max().unwrap_or(0);
        let last_time = log.iter().map(|event| event.time).max().unwrap_or(0);
        Self {
            inner: Lock::new(Inner {
                log,
                next_event,
                last_time,
                ..Inner::default()
            }),
        }
    }

    /// Number new events after `last` and stamp them no earlier than `time`, if the stream
    /// holds nothing later: for a stream whose store handed it only part of the log, which
    /// may leave out the last event stored. Ids are never used twice.
    #[must_use]
    pub fn continuing(self, last: EventId, time: u64) -> Self {
        self.inner.with(|inner| {
            inner.next_event = inner.next_event.max(last.get());
            inner.last_time = inner.last_time.max(time);
        });
        self
    }

    /// Read the time of each commit from `clock` (milliseconds since the Unix epoch) rather
    /// than from the system clock: for tests, and for hosts that keep time themselves.
    #[must_use]
    pub fn with_clock(self, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        let clock: Clock = Arc::new(clock);
        self.inner.with(|inner| inner.clock = Some(clock));
        self
    }

    /// Store every future commit in `log` before it is added to the stream.
    ///
    /// Events already in the stream are not written: a log is attached to an empty stream, or
    /// to one [`load`](Self::load)ed from that same log.
    #[must_use]
    pub fn with_log(self, log: impl Log + 'static) -> Self {
        let log: Box<dyn Log> = Box::new(log);
        self.inner.with(|inner| inner.store = Some(log));
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
        let drafts = drafts.into_iter().collect();
        let committed = self.inner.with(|inner| inner.commit(drafts))?;
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
        let topics = topics.into();
        let listener: Listener = Arc::new(listener);
        self.inner.with(|inner| {
            inner.next_listener += 1;
            let id = ListenerId(inner.next_listener);
            inner.subscriptions.push(Subscription {
                id,
                topics,
                listener,
            });
            id
        })
    }

    /// Stop delivering to a listener. Unknown ids are ignored.
    pub fn unsubscribe(&self, id: ListenerId) {
        self.inner.with(|inner| {
            inner
                .subscriptions
                .retain(|subscription| subscription.id != id);
        });
    }

    /// A page of the log: the first `query.limit` events that match `query`, oldest first.
    /// Complete whether or not the stream holds them: what it does not hold it asks its
    /// [`Log`] for (see the [module documentation](self)).
    ///
    /// # Errors
    ///
    /// Returns a [`LogError`] if the log could not be read. The stream stays open.
    pub fn read(&self, query: &Query) -> Result<Vec<Event>, LogError> {
        self.read_where(query, |_| true)
    }

    /// A page as [`read`](Self::read) gives, of the events for which `keep` also holds.
    /// The filter is applied before the limit, so a page is short only at the end of the log.
    /// It is called with the stream locked, so it must not use the stream.
    ///
    /// # Errors
    ///
    /// Returns a [`LogError`] if the log could not be read. The stream stays open.
    pub fn read_where(
        &self,
        query: &Query,
        keep: impl Fn(&Event) -> bool,
    ) -> Result<Vec<Event>, LogError> {
        let wanted = |event: &Event| query.selects(event) && keep(event);
        self.inner.with(|inner| {
            let start = inner.log.partition_point(|event| event.id <= query.after);
            let held = inner
                .log
                .iter()
                .skip(start)
                .filter(|event| wanted(event))
                .take(query.limit)
                .cloned();
            let mut page: BTreeMap<EventId, Event> = held.map(|event| (event.id, event)).collect();
            // The first `limit` of both together are among the first `limit` of each.
            let stored = match &mut inner.store {
                Some(store) => store.read(query, &wanted)?,
                None => Vec::new(),
            };
            let stored = stored
                .into_iter()
                .filter(|event| event.id > query.after && wanted(event));
            for event in stored {
                page.entry(event.id).or_insert(event);
            }
            Ok(page.into_values().take(query.limit).collect())
        })
    }

    /// A copy of every event so far, oldest first.
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        self.inner.with(|inner| inner.log.clone())
    }

    /// A copy of the event `id`, if the stream holds it.
    #[must_use]
    pub fn get(&self, id: EventId) -> Option<Event> {
        self.inner.with(|inner| {
            inner
                .log
                .binary_search_by_key(&id, |event| event.id)
                .ok()
                .and_then(|position| inner.log.get(position).cloned())
        })
    }

    /// How many causes lead up to the event `id`: zero for an event nothing caused. Stops
    /// counting at `limit`, so a long chain costs no more than that.
    #[must_use]
    pub fn depth(&self, id: EventId, limit: usize) -> usize {
        self.inner.with(|inner| {
            let cause_of = |id: EventId| {
                inner
                    .log
                    .binary_search_by_key(&id, |event| event.id)
                    .ok()
                    .and_then(|position| inner.log.get(position))
                    .and_then(|event| event.cause)
            };
            core::iter::successors(cause_of(id), |&cause| cause_of(cause))
                .take(limit)
                .count()
        })
    }

    /// A copy of the first `limit` events with an id above `after` for which `keep` holds,
    /// oldest first.
    #[must_use]
    pub fn after(&self, after: EventId, limit: usize, keep: impl Fn(&Event) -> bool) -> Vec<Event> {
        self.inner.with(|inner| {
            let start = inner.log.partition_point(|event| event.id <= after);
            inner
                .log
                .iter()
                .skip(start)
                .filter(|event| keep(event))
                .take(limit)
                .cloned()
                .collect()
        })
    }

    /// A copy of every event whose topic matches `topics`, oldest first.
    #[must_use]
    pub fn matching(&self, topics: &Pattern) -> Vec<Event> {
        self.inner.with(|inner| {
            inner
                .log
                .iter()
                .filter(|event| topics.matches(&event.topic))
                .cloned()
                .collect()
        })
    }

    /// A copy of every event that belongs to `session`, oldest first.
    #[must_use]
    pub fn session(&self, session: &SessionId) -> Vec<Event> {
        self.inner.with(|inner| {
            inner
                .log
                .iter()
                .filter(|event| event.session.as_ref() == Some(session))
                .cloned()
                .collect()
        })
    }

    /// How many events have been emitted.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.with(|inner| inner.log.len())
    }

    /// Whether nothing has been emitted yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use alloc::vec;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, PoisonError};

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
    fn a_loaded_stream_holds_the_log_and_continues_numbering() {
        let original = EventStream::new();
        original.emit(user("a")).unwrap();
        original.emit(user("b")).unwrap();

        let restored = EventStream::load(original.events());
        let next = restored.emit(user("c")).unwrap();

        assert_eq!(next.get(), 3);
        assert_eq!(restored.len(), 3);
        assert!(!restored.is_empty());
        assert_eq!(Source::Plugin("skills".into()).to_string(), "plugin:skills");
    }

    #[test]
    fn a_commit_has_one_time_and_time_never_runs_backwards() {
        let readings = Arc::new(Mutex::new(vec![30_u64, 10, 20]));
        let clock = Arc::clone(&readings);
        let stream = EventStream::new().with_clock(move || clock.lock().unwrap().pop().unwrap());

        stream.commit([user("a"), user("b")]).unwrap();
        stream.emit(user("c")).unwrap();
        stream.emit(user("d")).unwrap();

        // The clock read 20, then 10, then 30: the second commit keeps the first's time.
        let times: Vec<u64> = stream.events().iter().map(|event| event.time).collect();
        assert_eq!(times, [20, 20, 20, 30]);

        // A loaded stream carries on from the latest time it holds.
        let restored = EventStream::load(stream.events()).with_clock(|| 5);
        restored.emit(user("e")).unwrap();
        assert_eq!(restored.events()[4].time, 30);
    }

    #[test]
    fn events_after_an_id_are_read_in_pages() {
        let stream = EventStream::new();
        for topic in ["a", "b", "a", "a"] {
            stream.emit(user(topic)).unwrap();
        }
        let topic_a = |event: &Event| event.topic == "a";

        let first = stream.after(EventId::new(0), 2, topic_a);
        let ids: Vec<u64> = first.iter().map(|event| event.id.get()).collect();
        assert_eq!(ids, [1, 3]);
        let rest = stream.after(EventId::new(3), 10, topic_a);
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].id.get(), 4);
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
    fn a_commit_of_nothing_stores_nothing() {
        let recorder = Recorder::default();
        let (batches, broken) = (Arc::clone(&recorder.batches), Arc::clone(&recorder.broken));
        let stream = EventStream::new().with_log(recorder);

        assert_eq!(stream.commit([]).unwrap(), []);
        stream.emit(user("message")).unwrap();
        assert_eq!(stream.commit([]).unwrap(), []);

        // The log was handed the one event, and never an empty batch.
        assert_eq!(batches.lock().unwrap().as_slice(), [stream.events()]);
        // A stream that is closed still says so, whatever it is asked to store.
        broken.store(1, Ordering::SeqCst);
        stream.emit(user("lost")).unwrap_err();
        stream.commit([]).unwrap_err();
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

    /// A store that hands the stream nothing when it is opened and answers every read from
    /// what it keeps, as a store that reads old sessions on request does.
    struct Shelf {
        kept: Vec<Event>,
        broken: Arc<AtomicUsize>,
    }

    impl Log for Shelf {
        fn commit(&mut self, batch: &[Event]) -> Result<(), LogError> {
            self.kept.extend_from_slice(batch);
            Ok(())
        }

        fn read(
            &mut self,
            query: &Query,
            keep: &dyn Fn(&Event) -> bool,
        ) -> Result<Vec<Event>, LogError> {
            if self.broken.load(Ordering::SeqCst) > 0 {
                return Err(LogError::new("unreadable"));
            }
            Ok(self
                .kept
                .iter()
                .filter(|event| event.id > query.after && query.selects(event) && keep(event))
                .take(query.limit)
                .cloned()
                .collect())
        }
    }

    #[test]
    fn a_read_is_paged_and_narrowed_by_session_and_topic() {
        let stream = EventStream::new();
        let main = SessionId::new("main");
        for topic in ["message", "usage", "message", "message"] {
            stream.emit(user(topic).session(main.clone())).unwrap();
        }
        stream.emit(user("message")).unwrap();
        let ids = |events: Vec<Event>| events.iter().map(|e| e.id.get()).collect::<Vec<_>>();

        let query = Query::first(2).session(main).topics("message");
        let first = stream.read(&query).unwrap();
        let rest = stream.read(&query.after(first[1].id)).unwrap();

        assert_eq!((ids(first), ids(rest)), (vec![1, 3], vec![4]));
        let odd = stream.read_where(&Query::first(10), |event| event.id.get() % 2 == 1);
        assert_eq!(ids(odd.unwrap()), [1, 3, 5]);
    }

    #[test]
    fn a_read_reaches_the_store_for_what_the_stream_does_not_hold() {
        // The log holds five events; the store hands the stream only the two of no session.
        let old = EventStream::new();
        let side = SessionId::new("side");
        old.emit(Draft::new(Source::Host, "plugin/activated"))
            .unwrap();
        old.emit(user("message").session(side.clone())).unwrap();
        old.emit(Draft::new(Source::Host, "plugin/activated"))
            .unwrap();
        old.emit(user("message").session(side.clone())).unwrap();
        old.emit(user("message").session(side.clone())).unwrap();
        let (last, time) = (EventId::new(5), old.events()[4].time);
        let broken = Arc::new(AtomicUsize::new(0));
        let shelf = Shelf {
            kept: old.events(),
            broken: Arc::clone(&broken),
        };
        let held: Vec<Event> = old
            .events()
            .into_iter()
            .filter(|event| event.session.is_none())
            .collect();
        let stream = EventStream::load(held)
            .continuing(last, time)
            .with_log(shelf);
        let ids = |events: Vec<Event>| events.iter().map(|e| e.id.get()).collect::<Vec<_>>();

        // Numbering carries on after the last event stored, which the stream never held.
        let next = stream.emit(user("message").session(side.clone())).unwrap();
        assert_eq!(next.get(), 6);
        // The convenient read answers from memory; the paged one is complete, once each.
        assert_eq!(ids(stream.session(&side)), [6]);
        let page = stream.read(&Query::first(10).session(side)).unwrap();
        assert_eq!(ids(page), [2, 4, 5, 6]);
        assert_eq!(ids(stream.read(&Query::first(3)).unwrap()), [1, 2, 3]);

        // A store that cannot be read fails the read, not the stream.
        broken.store(1, Ordering::SeqCst);
        let error = stream.read(&Query::first(10)).unwrap_err();
        assert_eq!(error.to_string(), "event log: unreadable");
        assert!(stream.emit(user("later")).is_ok());
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
