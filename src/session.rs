//! The conversation as a projection of the event stream.
//!
//! The loop never changes a conversation in place. Each step returns [`Effect`]s, and the
//! [`Journal`] turns them into events: the conversation a model sees is whatever the
//! `message` events add up to. A run can therefore be resumed, read back or inspected from
//! the log alone, and every plugin sees the same history the model does.
//!
//! One stream holds many sessions. Each begins with a `session/start` event, and every event
//! below names the session it belongs to, so a session is rebuilt from its own events alone.
//!
//! | Topic | Source | Payload |
//! | --- | --- | --- |
//! | `session/start` | `host` | `{"parent", "task"}`, each only if there is one |
//! | `message` | `user`, `agent` or `environment` | the message, in the shape [`crate::message`] reads and writes |
//! | `tool/call` | `agent` | `{"id", "tool", "input"}`, caused by its `message` |
//! | `gate/decision` | `host` | `{"point", "id", "tool", "verdict", "reason", "gate", "caller"}`, caused by its `tool/call`: what the gates said |
//! | `gate/answer` | `user` | `{"id", "tool", "allow", "reason"}`, caused by its `tool/call`: a person's answer to a gate that asked |
//! | `tool/start` | `host` | `{"id", "tool"}`, caused by its `tool/call`: the tool is about to run |
//! | `tool/result` | `environment` | `{"id", "tool", "ok", "output"}`, and `"content"` when the result is not one block of text, caused by its `tool/call`; with `"signal"` where the call waited and this is what the signal of that name brought |
//! | `model/request` | `host` | `{"provider", "model", "messages", "tools"}`: the model is about to be asked, with that many messages and tools; what follows it in the session (the model's `message`, or a `turn/end`) says when the answer came |
//! | `model/retry` | `host` | `{"provider", "model", "attempt", "kind", "reason", "delay_ms", "by"}`: an attempt at the request failed for `reason` (of `kind`, as a failed `turn/end` names them) and it is made again after `delay_ms`; `attempt` counts the retries from 1, and `by` says who retries (`provider`) |
//! | `model/progress` | `host` | `{"provider", "model", "tokens", "elapsed_ms"}` with `"text"`, `"thinking"` and `"calls"` (each `{"id", "tool", "input"}`) where there is some: a checkpoint of an answer the model is still writing, holding what it wrote since the checkpoint before; `tokens` is what the attempt has written so far, with `"estimated": true` where that is reckoned from the length of the text; not part of the conversation |
//! | `model/partial` | `host` | the same fields, holding the whole of what the model had written when its request ended without an answer, and `"why"`: `stopped`, `time-limit` or `failed` (with the `"kind"` a failed `turn/end` names); not part of the conversation, and nothing in it is run |
//! | `usage` | `agent` | `{"model", "input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens", "reasoning_tokens", "stop"}`, each count only if the provider gave it; the cache counts are part of `input_tokens` and the reasoning count of `output_tokens`; `stop` is why the model stopped (`finished`, `limit`, `tool_calls` or `refused`), if the provider said |
//! | `turn/end` | `host` | `{"status"}`: `finished`, `cut-off` (the answer stopped at the most the model may write), `turn-limit`, `time-limit`, `output-limit`, `failed` (with `"reason"`, and `"kind"` where the model gave no answer: `context-overflow`, `rate-limit`, `auth`, `timeout`, `unreachable` or `other`), `stopped` or `yielded` |
//! | `run/restart` | `host` | `{"attempt"}`: an unfinished run was picked up again |
//! | `run/stop` | `user` | `{}`: a person asked the run to stop |
//! | `tool/wait` | `host` | `{"id", "tool", "signal"}`, caused by its `tool/call`: the call's result comes with the signal |
//! | `signal` | `environment` | `{"name", "payload"}`: what a waiting call was waiting for; the `tool/result` of each call that waited follows it in the same commit |
//! | `compaction` | `host` | `{"summary", "keep_from", "by"}`: the messages before the `message` event `keep_from` are summarised |
//!
//! Two belong to no session: each process that opens the log records what it runs as, and a
//! session removed from the log leaves a marker.
//!
//! | Topic | Source | Payload |
//! | --- | --- | --- |
//! | `process/start` | `host` | `{"program", "version", "world", "platform"}`, `world` only if the process hosts add-ons: see [`ProcessInfo`] |
//! | `session/removed` | `user` | `{"session"}`: a person removed that session's events from the log, with the sessions it started: see [`removal`] |
//!
//! While a program runs the log only grows. Between runs a store may remove whole sessions,
//! as maintenance a person asks for, and nothing smaller. Ids are never used again, so a
//! reader meets gaps in them, a `cause` naming an event no longer there, and a session whose
//! parent is gone (when its files were edited by hand); none of these is an error.
//!
//! A tool call leaves a trail of its own, one commit at a time: `tool/call` with the model's
//! message, `tool/start` just before the tool runs, `tool/result` as soon as it returns, and
//! only then the `message` that carries the results of every call of that step to the
//! model. After a crash the log therefore says of each call whether it never started, was
//! running, or finished with a result that only needs handing over.
//!
//! `output` is the result's text. A result that is anything but one block of text (an image,
//! several blocks, none) also has its blocks as `content`, so that a run picked up after a
//! crash hands the model exactly what the tool returned.
//!
//! Where a gate guards tool calls, its decision on each call is stored before the tool can
//! start. A call a gate asked about waits for a `gate/answer`.
//!
//! A run is *unfinished* from its first `message` until its `turn/end`, which says how it
//! ended. A session whose events stop short of that was interrupted, and
//! [`Harness::continue_run`] picks it up, unless it is *waiting*: a gate asked about one of
//! its calls and nobody has answered yet. [`Harness::answer`] gives the answer and takes the
//! run on.
//!
//! A tool call may *wait*: whatever served it said, through
//! [`corduroy_core::call::wait_for`], that its result will come with a signal. The call
//! gets a placeholder result and a `tool/wait`, and the run stops before the model is asked
//! again; when a `signal` of that name is stored, its payload takes the placeholder's place
//! and the run can go on. The call's `tool/result` is stored with the signal, so every
//! call has one; a log from before that has the `signal` alone, and reads the same. A child session's end is such a signal (`session/<id>/end`), so a
//! parent run waits for a child in the log, as it waits for a person's answer.
//!
//! A `compaction` stands a summary in for the messages before `keep_from`. They stay in the
//! conversation, and in the log, but are hidden from the model: it sees the summary, a short
//! acknowledgement, and the messages from `keep_from` on. Which messages to summarise, and
//! how, is the business of the plugin that provides the [`COMPACTION`] service; see
//! [`CompactOperation`](crate::compact::CompactOperation).
//!
//! [`Harness::continue_run`]: crate::Harness::continue_run
//! [`Harness::answer`]: crate::Harness::answer

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::btree_map::Entry;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec::Vec;

use anyhow::{Context as _, Result};
use corduroy_core::ModelRequest;
use corduroy_core::sync::Lock;
use corduroy_core::{
    BYTES_PER_TOKEN, Content, Decision, Draft, Event, EventId, EventStream, LogError, Message,
    Part, Query, Role, SessionId, Source, StopReason, TOOL_CALL, ToolCall, ToolOutput, ToolResult,
    Usage, Verdict,
};
use serde_json::{Value, json};

use crate::harness::HarnessError;
use crate::message::{decode, decode_content, encode, encode_content};
use crate::provider::{ProviderError, ProviderErrorKind, Retry};

/// Topic of the events that make up the conversation.
pub const MESSAGE: &str = "message";

/// Topic of the event that ends a run.
const TURN_END: &str = "turn/end";

/// Topic of the event that reports what a model request cost.
const USAGE: &str = "usage";

/// Topic of the event that records a request going out to the model.
pub const MODEL_REQUEST: &str = "model/request";

/// Topic of the event that records a model request being made again after a failed attempt.
pub const MODEL_RETRY: &str = "model/retry";

/// Topic of the event that checkpoints what a model has written while it is still writing.
pub const MODEL_PROGRESS: &str = "model/progress";

/// Topic of the event that keeps what a model had written when its request ended without
/// an answer.
pub const MODEL_PARTIAL: &str = "model/partial";

/// Topic of the event that records an unfinished run being picked up again.
const RESTART: &str = "run/restart";

/// Topic of the event that records a person asking a run to stop.
pub const STOP: &str = "run/stop";

/// Topic of the event that records what the gates decided about a tool call.
const GATE_DECISION: &str = "gate/decision";

/// Topic of the event that records a person's answer to a gate that asked.
const GATE_ANSWER: &str = "gate/answer";

/// Topic of the event that begins a session.
pub const SESSION_START: &str = "session/start";

/// Topic of the event that records that a tool call waits for a signal.
const TOOL_WAIT: &str = "tool/wait";

/// Topic of the event that brings what waiting calls wait for.
pub const SIGNAL: &str = "signal";

/// The signal a child session's end raises, with its outcome as the payload.
#[must_use]
pub fn end_of(session: &SessionId) -> String {
    format!("session/{session}/end")
}

/// The session whose end `signal` is, if it is one.
#[must_use]
pub fn ended(signal: &str) -> Option<SessionId> {
    signal
        .strip_prefix("session/")
        .and_then(|rest| rest.strip_suffix("/end"))
        .map(SessionId::new)
}

/// How many events one read of the log asks for.
const PAGE: usize = 1000;

/// Every event `query` and `keep` select, read a page at a time, so that a store that keeps
/// part of the log out of memory is asked for it.
fn read_all(
    events: &EventStream,
    mut query: Query,
    keep: impl Fn(&Event) -> bool,
) -> Result<Vec<Event>, LogError> {
    let mut all = Vec::new();
    loop {
        let page = events.read_where(&query, &keep)?;
        let full = page.len() >= query.limit;
        let Some(last) = page.last().map(|event| event.id) else {
            return Ok(all);
        };
        all.extend(page);
        if !full {
            return Ok(all);
        }
        query = query.after(last);
    }
}

/// Topic of the event that stands a summary in for the messages before a point.
pub const COMPACTION: &str = "compaction";

/// What the model is told about a summary that replaced the start of the conversation.
const SUMMARY_INTRO: &str = "The start of this conversation was condensed to save space. This \
                             summary takes its place:";

/// The model's side of the summary, so the conversation still alternates.
const SUMMARY_ACK: &str = "Understood. I will continue from that summary.";

/// Topic of the marker a session leaves when it is removed from the log.
pub const SESSION_REMOVED: &str = "session/removed";

/// What removing a session from the log takes. See [`removal`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Removal {
    /// The sessions whose events go: the one asked for and every session started from it,
    /// directly or through others.
    pub sessions: BTreeSet<SessionId>,
    /// The markers recording it, one for each session, to be stored as one commit after
    /// what is kept.
    pub markers: Vec<Draft>,
}

/// What removing `session` from the log behind `events` takes.
///
/// A store that removes sessions asks this of the events it holds, so that which sessions go
/// and what the record says of it are the same whatever the store.
///
/// # Errors
///
/// Returns [`HarnessError::UnknownSession`] if the log holds no such session, or
/// [`HarnessError::Log`] if it could not be read.
pub fn removal(events: &EventStream, session: &SessionId) -> Result<Removal, HarnessError> {
    let sessions = SessionInfo::read(events)?;
    if !sessions.iter().any(|info| info.id == *session) {
        return Err(HarnessError::UnknownSession(session.clone()));
    }
    let mut removed = BTreeSet::from([session.clone()]);
    // Started in order, so a session's parent is always met before it.
    for info in &sessions {
        if info
            .parent
            .as_ref()
            .is_some_and(|parent| removed.contains(parent))
        {
            removed.insert(info.id.clone());
        }
    }
    let markers = removed
        .iter()
        .map(|id| {
            Draft::new(Source::User, SESSION_REMOVED)
                .payload(json!({"session": id.as_str()}).to_string())
        })
        .collect();
    Ok(Removal {
        sessions: removed,
        markers,
    })
}

/// Topic of the event a process records when it opens the log: what it runs as.
pub const PROCESS_START: &str = "process/start";

/// What a process that opens a log runs as, recorded once by each as `process/start`.
///
/// Whoever reads the log, or resumes a run from it, can then tell whether it was written by
/// the same build on the same platform: resuming is expected to work there, and is best
/// effort anywhere else.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProcessInfo {
    /// The program's name, such as `corduroy`.
    pub program: String,
    /// The program's version.
    pub version: String,
    /// The plugin world it hosts add-ons against, such as `corduroy:plugin@0.1.0`; none for
    /// a process without add-ons.
    pub world: Option<String>,
    /// What it runs on, as `<architecture>-<operating system>`, such as `x86_64-linux`.
    pub platform: String,
}

impl ProcessInfo {
    /// The program `program` at `version`, on `platform`, with no add-ons.
    #[must_use]
    pub fn new(
        program: impl Into<String>,
        version: impl Into<String>,
        platform: impl Into<String>,
    ) -> Self {
        Self {
            program: program.into(),
            version: version.into(),
            world: None,
            platform: platform.into(),
        }
    }

    /// Record that the process hosts add-ons built against the plugin world `world`.
    #[must_use]
    pub fn world(mut self, world: impl Into<String>) -> Self {
        self.world = Some(world.into());
        self
    }

    /// The `process/start` event recording this.
    #[must_use]
    pub fn start(&self) -> Draft {
        let mut payload = json!({
            "program": self.program,
            "version": self.version,
            "platform": self.platform,
        });
        if let (Some(world), Some(fields)) = (&self.world, payload.as_object_mut()) {
            fields.insert("world".to_owned(), json!(world));
        }
        Draft::new(Source::Host, PROCESS_START).payload(payload.to_string())
    }
}

/// Name of the session a harness starts with, and the one [`Harness::run`] uses.
///
/// [`Harness::run`]: crate::Harness::run
pub const MAIN: &str = "main";

/// A session and where it came from, as its `session/start` event recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    /// The session.
    pub id: SessionId,
    /// The session that started it, if one did.
    pub parent: Option<SessionId>,
    /// What it was started to do, for a session started by another for a task.
    pub task: Option<String>,
}

impl SessionInfo {
    /// The event that begins this session.
    pub(crate) fn start(&self) -> Draft {
        let mut payload = serde_json::Map::new();
        if let Some(parent) = &self.parent {
            payload.insert("parent".to_owned(), json!(parent.as_str()));
        }
        if let Some(task) = &self.task {
            payload.insert("task".to_owned(), json!(task));
        }
        let payload = Value::Object(payload);
        Draft::new(Source::Host, SESSION_START)
            .session(self.id.clone())
            .payload(payload.to_string())
    }

    /// Every session the log behind `events` holds, in the order they were started.
    ///
    /// # Errors
    ///
    /// Returns a [`LogError`] if the log could not be read.
    pub fn read(events: &EventStream) -> Result<Vec<Self>, LogError> {
        // Only the harness starts sessions; a plugin's `session/start` is not one.
        let starts = read_all(events, Query::first(PAGE).topics(SESSION_START), |event| {
            event.source == Source::Host
        })?;
        Ok(starts
            .into_iter()
            .filter_map(|event| {
                let payload: Value = serde_json::from_str(&event.payload).ok()?;
                let parent = payload.get("parent").and_then(Value::as_str);
                let task = payload.get("task").and_then(Value::as_str);
                Some(Self {
                    id: event.session?,
                    parent: parent.map(SessionId::new),
                    task: task.map(str::to_owned),
                })
            })
            .collect())
    }
}

/// A session the table knows of. Its journal is built from the stream when first needed.
struct Known {
    info: SessionInfo,
    journal: Option<Arc<Journal>>,
}

/// The sessions of one event stream and the journal of each.
///
/// The table is a projection of the stream's `session/start` events, kept so that a step
/// of the loop can find the journal of the session it is working in.
pub struct Sessions {
    events: Arc<EventStream>,
    known: Lock<BTreeMap<SessionId, Known>>,
}

impl core::fmt::Debug for Sessions {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("Sessions").field(&self.list()).finish()
    }
}

impl Sessions {
    /// The sessions `events` holds, with the session [`MAIN`] started if it is not among
    /// them.
    ///
    /// # Errors
    ///
    /// Returns a [`LogError`] if the start of the main session cannot be stored.
    pub fn load(events: Arc<EventStream>) -> Result<Self, LogError> {
        let unloaded = |info| Known {
            info,
            journal: None,
        };
        let mut known: BTreeMap<SessionId, Known> = SessionInfo::read(&events)?
            .into_iter()
            .map(|info| (info.id.clone(), unloaded(info)))
            .collect();
        if let Entry::Vacant(vacant) = known.entry(SessionId::new(MAIN)) {
            let info = SessionInfo {
                id: vacant.key().clone(),
                parent: None,
                task: None,
            };
            events.emit(info.start())?;
            vacant.insert(unloaded(info));
        }
        Ok(Self {
            events,
            known: Lock::new(known),
        })
    }

    /// Start a session called `id`, recording `parent` as the session it came from.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::DuplicateSession`] if `id` is taken,
    /// [`HarnessError::UnknownSession`] if `parent` does not exist, or [`HarnessError::Log`]
    /// if the start could not be stored.
    pub fn start(
        &self,
        id: SessionId,
        parent: Option<&SessionId>,
    ) -> Result<SessionId, HarnessError> {
        self.start_with(id, parent, None)
    }

    /// Start a session as [`start`](Self::start) does, recording what it is for.
    ///
    /// # Errors
    ///
    /// As [`start`](Self::start).
    pub(crate) fn start_with(
        &self,
        id: SessionId,
        parent: Option<&SessionId>,
        task: Option<String>,
    ) -> Result<SessionId, HarnessError> {
        let info = SessionInfo {
            id,
            parent: parent.cloned(),
            task,
        };
        self.known.with(|known| {
            if known.contains_key(&info.id) {
                return Err(HarnessError::DuplicateSession(info.id));
            }
            if let Some(parent) = parent.filter(|parent| !known.contains_key(parent)) {
                return Err(HarnessError::UnknownSession(parent.clone()));
            }
            // Stored while the table is locked, so two callers cannot both start the same id.
            // A listener for `session/start` must therefore not come back to this table.
            self.events.emit(info.start())?;
            let id = info.id.clone();
            known.insert(
                id.clone(),
                Known {
                    info,
                    journal: None,
                },
            );
            Ok(id)
        })
    }

    /// Every session, in order of id.
    #[must_use]
    pub fn list(&self) -> Vec<SessionInfo> {
        self.known
            .with(|known| known.values().map(|known| known.info.clone()).collect())
    }

    /// The journal of `session`, rebuilt from the stream the first time it is asked for.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::UnknownSession`] if there is no such session, or
    /// [`HarnessError::Run`] if its events cannot be decoded.
    pub fn journal(&self, session: &SessionId) -> Result<Arc<Journal>, HarnessError> {
        self.known.with(|known| {
            let entry = known
                .get_mut(session)
                .ok_or_else(|| HarnessError::UnknownSession(session.clone()))?;
            if let Some(journal) = &entry.journal {
                return Ok(Arc::clone(journal));
            }
            let journal = Arc::new(Journal::new(session.clone(), Arc::clone(&self.events))?);
            entry.journal = Some(Arc::clone(&journal));
            Ok(journal)
        })
    }
}

/// One conversation, as a step of the loop sees it.
#[derive(Debug, Clone)]
pub struct Session {
    id: SessionId,
    messages: Vec<Message>,
    /// The summary that stands in for the messages before index `.1`, for the model.
    summary: Option<(String, usize)>,
    /// Why the model stopped the last time it answered, if its provider said.
    stopped: Option<StopReason>,
    /// The output tokens of the current turn's model requests.
    written: u64,
}

impl Session {
    /// The session's id.
    #[must_use]
    pub const fn id(&self) -> &SessionId {
        &self.id
    }

    /// Every message, oldest first, as a person sees the conversation: what a summary
    /// stands in for included.
    #[must_use]
    pub fn conversation(&self) -> &[Message] {
        &self.messages
    }

    /// The conversation as the model is sent it: a summary in place of what it stands for,
    /// and messages of the same role one after another joined into one, so roles alternate.
    #[must_use]
    pub fn for_model(&self) -> Vec<Message> {
        let mut view: Vec<Message> = Vec::new();
        let kept = match &self.summary {
            Some((summary, from)) => {
                view.push(summary_message(summary));
                view.push(Message::assistant().text(SUMMARY_ACK));
                self.messages.get(*from..).unwrap_or_default()
            }
            None => &self.messages,
        };
        for message in kept.iter().filter(|message| !message.is_empty()) {
            match view.last_mut() {
                Some(last) if last.role == message.role => {
                    last.parts.extend(message.parts.iter().cloned());
                }
                _ => view.push(message.clone()),
            }
        }
        view
    }

    /// The messages of the current turn: from the last one a person wrote on.
    #[must_use]
    pub fn turn(&self) -> &[Message] {
        let start = self
            .messages
            .iter()
            .rposition(|message| message.role == Role::User && !message.is_tool_results())
            .unwrap_or(0);
        self.messages.get(start..).unwrap_or_default()
    }

    /// The tool calls of the current turn that have no result yet, in the order the model
    /// asked for them.
    #[must_use]
    pub fn pending_calls(&self) -> Vec<ToolCall> {
        let turn = self.turn();
        let answered: Vec<&str> = turn
            .iter()
            .flat_map(Message::tool_results)
            .map(|result| result.id.as_str())
            .collect();
        let mut seen: Vec<&str> = Vec::new();
        turn.iter()
            .flat_map(Message::tool_calls)
            .filter(|call| !answered.contains(&call.id.as_str()))
            .filter(|call| {
                let first = !seen.contains(&call.id.as_str());
                seen.push(&call.id);
                first
            })
            .cloned()
            .collect()
    }

    /// How many times the model has answered in the current turn.
    #[must_use]
    pub fn model_turns(&self) -> usize {
        self.turn()
            .iter()
            .filter(|message| message.role == Role::Assistant)
            .count()
    }

    /// Why the model stopped the last time it answered, if its provider said: whether it
    /// finished, asked for tools, was cut off at the most it may write, or declined. A step
    /// of the loop can act on it, say by asking the model to go on after a cut-off answer.
    #[must_use]
    pub const fn stopped(&self) -> Option<StopReason> {
        self.stopped
    }

    /// How many tokens the model has written in the current turn, thinking included, as
    /// its provider counted them: what a run's
    /// [output limit](crate::Harness::with_output_limit) is held against.
    #[must_use]
    pub const fn turn_output_tokens(&self) -> u64 {
        self.written
    }

    /// The model's last answer, if the conversation ends with one that asks for nothing.
    #[must_use]
    pub fn answer(&self) -> Option<String> {
        let last = self.messages.last()?;
        (last.role == Role::Assistant && last.tool_calls().next().is_none())
            .then(|| last.text_content())
    }
}

/// The message that stands in for what `summary` summarises.
fn summary_message(summary: &str) -> Message {
    Message::user().text(format!("{SUMMARY_INTRO}\n<summary>\n{summary}\n</summary>"))
}

/// Something a step of the loop wants recorded.
#[derive(Debug)]
#[non_exhaustive]
pub enum Effect {
    /// Append a message to the conversation.
    Message(Box<Message>),
    /// Note what a model request cost.
    Usage {
        /// The model asked.
        model: String,
        /// What it cost.
        usage: Usage,
        /// Why the model stopped, if its provider said.
        stop: Option<StopReason>,
    },
    /// Note what the gates decided about a tool call the model asked for.
    Gated {
        /// The tool call's id.
        call: String,
        /// The strictest verdict and the gate that gave it.
        decision: Decision,
    },
    /// Stand `summary` in for the messages before the `message` event `keep_from`.
    Compacted {
        /// What the hidden messages said.
        summary: String,
        /// The first message the model still sees.
        keep_from: EventId,
        /// Who wrote the summary.
        by: String,
    },
}

impl From<Message> for Effect {
    fn from(message: Message) -> Self {
        Self::Message(Box::new(message))
    }
}

/// A person's answer to a gate that asked about a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Let the call run.
    Allow,
    /// Do not run it. The reason goes to the model.
    Deny(String),
}

/// A tool call that waits for a person's answer because a gate asked about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    /// The session the call is in: the one asked about, or a child session it waits for.
    pub session: SessionId,
    /// The tool call's id, by which it is answered.
    pub call: String,
    /// The tool the model asked for.
    pub tool: String,
    /// The input the model gave it, as JSON text.
    pub input: String,
    /// Why the gate asked.
    pub reason: String,
    /// The gate that asked.
    pub gate: String,
}

/// A tool call whose result will come with a signal. See [`corduroy_core::call`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wait {
    /// The session the call is in.
    pub session: SessionId,
    /// The tool call's id.
    pub call: String,
    /// The tool that was called.
    pub tool: String,
    /// The signal it waits for.
    pub signal: String,
}

/// How a run ended, as its `turn/end` records it. A [`Loop`](crate::Loop) ends a run with
/// one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum End {
    /// The model answered.
    Finished,
    /// The model answered, and its answer was cut off at the most it may write.
    CutOff,
    /// The model was still working when it ran out of turns.
    TurnLimit,
    /// The model was still working when the run ran out of time
    /// ([`Harness::with_time_limit`](crate::Harness::with_time_limit)).
    TimeLimit,
    /// The model was still working when the run had written all it may
    /// ([`Harness::with_output_limit`](crate::Harness::with_output_limit)).
    OutputLimit,
    /// The model, or the run, failed, for this reason.
    Failed(String),
    /// The model gave no answer, for this reason. Recorded as a failed run, with the
    /// error's [kind](crate::ProviderError::kind) beside its reason.
    ModelFailed(ProviderError),
    /// A person stopped it.
    Stopped,
    /// It stopped with nothing left to do and no answer.
    Yielded,
}

impl End {
    /// Whether the run ended because its request was more than the model holds.
    pub(crate) fn overflowed(&self) -> bool {
        matches!(
            self,
            Self::ModelFailed(error) if error.kind() == ProviderErrorKind::ContextOverflow
        )
    }

    /// How a run with nothing left to do ends: finished if the model has answered `session`,
    /// cut off if that answer stopped at the most the model may write, yielded if there is
    /// none.
    #[must_use]
    pub fn settled(session: &Session) -> Self {
        match (session.answer(), session.stopped()) {
            (Some(_), Some(StopReason::Limit)) => Self::CutOff,
            (Some(_), _) => Self::Finished,
            (None, _) => Self::Yielded,
        }
    }

    fn payload(&self) -> Value {
        match self {
            Self::Finished => json!({"status": "finished"}),
            Self::CutOff => json!({"status": "cut-off"}),
            Self::TurnLimit => json!({"status": "turn-limit"}),
            Self::TimeLimit => json!({"status": "time-limit"}),
            Self::OutputLimit => json!({"status": "output-limit"}),
            Self::Failed(reason) => json!({"status": "failed", "reason": reason}),
            Self::ModelFailed(error) => json!({
                "status": "failed",
                "reason": error.to_string(),
                "kind": error.kind().name(),
            }),
            Self::Stopped => json!({"status": "stopped"}),
            Self::Yielded => json!({"status": "yielded"}),
        }
    }

    /// What a `turn/end` payload says, or `None` for one that says nothing (as those
    /// written before runs recorded how they ended).
    fn read(payload: &str) -> Option<Self> {
        let payload: Value = serde_json::from_str(payload).ok()?;
        Some(match payload.get("status")?.as_str()? {
            "finished" => Self::Finished,
            "cut-off" => Self::CutOff,
            "turn-limit" => Self::TurnLimit,
            "time-limit" => Self::TimeLimit,
            "output-limit" => Self::OutputLimit,
            "failed" => {
                let reason = payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                payload.get("kind").and_then(Value::as_str).map_or_else(
                    || Self::Failed(reason.to_owned()),
                    |kind| {
                        let kind = ProviderErrorKind::named(kind);
                        Self::ModelFailed(ProviderError::new(reason).of(kind))
                    },
                )
            }
            "stopped" => Self::Stopped,
            _ => Self::Yielded,
        })
    }
}

/// What a waiting call's result is until its signal comes. The model never sees it: a run
/// with a waiting call stops before the model is asked again.
fn placeholder(signal: &str) -> String {
    format!("[waiting for {signal}]")
}

/// What the log holds about the gates' view of one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Gated {
    /// The strictest verdict the gates gave.
    pub(crate) verdict: Verdict,
    /// The gate that gave it.
    pub(crate) gate: String,
    /// A person's answer, if the gate asked and someone has answered.
    pub(crate) answer: Option<Answer>,
}

/// A tool call the model asked for whose response is not in the conversation yet.
#[derive(Clone)]
struct OpenCall {
    /// Its `tool/call` event.
    event: EventId,
    tool: String,
    /// The input the model gave, as JSON text.
    input: String,
    /// What the gates decided, once a `gate/decision` was stored for it.
    gated: Option<Gated>,
    /// Whether a `tool/start` was stored for it.
    started: bool,
    /// Whether it succeeded and what it returned, once a `tool/result` was stored for it.
    result: Option<(bool, ToolOutput)>,
}

/// How far the log says a tool call got, for one whose response is not in the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Progress {
    /// The tool was never started.
    NotStarted,
    /// The tool was started and no result was stored: it may or may not have run.
    Started,
    /// The tool returned and its result is in the log.
    Finished {
        /// Whether the call succeeded.
        ok: bool,
        /// What the tool returned, every block of it.
        output: ToolOutput,
    },
}

struct State {
    messages: Vec<Message>,
    /// The `message` event each of `messages` came from, in the same order.
    message_ids: Vec<EventId>,
    /// The latest summary, and the first `message` event it does not stand for.
    compaction: Option<(String, EventId)>,
    /// The input tokens of the last model request, unless a compaction came after it.
    input_tokens: u64,
    /// The tokens the model wrote in answer to that request, which the next one sends back.
    replied: u64,
    /// The bytes of the messages added since that request was answered.
    added: usize,
    /// Whether the last run ended because its request was more than the model holds, and
    /// nothing has been answered or summarised since.
    overflowed: bool,
    /// Why the model stopped the last time it answered, if its provider said.
    stopped: Option<StopReason>,
    /// The output tokens of the model requests since a person last wrote.
    written: u64,
    /// Tool calls that waited for a signal, by tool-call id, with the payload once it came.
    waits: BTreeMap<String, Waited>,
    /// The tool calls waiting for a response, by tool-call id.
    calls: BTreeMap<String, OpenCall>,
    /// Whether a run has begun and not reached its `turn/end`.
    unfinished: bool,
    /// How many times the unfinished run has been picked up again.
    restarts: u32,
    /// How the last run ended, if it recorded that.
    ended: Option<End>,
}

/// A tool call that waited for a signal.
struct Waited {
    tool: String,
    /// The call's `tool/call` event, if the call was open when it began to wait.
    event: Option<EventId>,
    signal: String,
    /// The signal's payload, once it came.
    payload: Option<String>,
}

impl Waited {
    /// The `tool/result` of the call `call`, which waited for this: what its signal
    /// brought as `payload`, caused by the call where that is known.
    fn brought(&self, call: &str, payload: &str) -> Draft {
        let mut result = result_payload(call, &self.tool, true, &ToolOutput::text(payload));
        if let Some(fields) = result.as_object_mut() {
            fields.insert("signal".to_owned(), json!(self.signal));
        }
        let draft = Draft::new(Source::Environment, "tool/result").payload(result.to_string());
        match self.event {
            Some(event) => draft.cause(event),
            None => draft,
        }
    }
}

/// One step's worth of events, and what they change in the projection once stored.
#[derive(Default)]
struct Batch {
    drafts: Vec<Draft>,
    /// Messages, each with the position of its `message` draft.
    messages: Vec<(usize, Message)>,
    /// Compactions, each with the position of its draft, the summary and `keep_from`.
    compactions: Vec<(usize, String, EventId)>,
    /// Tool-call id to the position of its `tool/call` draft, the tool's name and its input.
    opened: BTreeMap<String, (usize, String, String)>,
    /// Ids of the tool calls answered in this batch.
    closed: Vec<String>,
    /// What the gates decided about tool calls, by tool-call id.
    gated: Vec<(String, Gated)>,
    /// A model request recorded in this batch.
    asked: Option<Asked>,
}

/// What a model request came to.
struct Asked {
    /// The tokens it read.
    input: u64,
    /// The tokens the model wrote.
    output: u64,
    /// Why the model stopped, if its provider said.
    stopped: Option<StopReason>,
}

impl Batch {
    /// Add the `tool/call` event for `call`, caused by the message at `message`.
    fn call(&mut self, call: &ToolCall, message: usize) {
        let input: Value = serde_json::from_str(&call.input).unwrap_or_else(|_| json!({}));
        let payload = json!({"id": call.id, "tool": call.name, "input": input});
        self.opened.insert(
            call.id.clone(),
            (self.drafts.len(), call.name.clone(), input.to_string()),
        );
        self.drafts.push(
            Draft::new(Source::Agent, "tool/call")
                .payload(payload.to_string())
                .cause_in_batch(message),
        );
    }

    /// Add the `tool/result` event for `result`, unless the `open` call it answers already
    /// has one from when the tool returned. The event is caused by its call, which is in
    /// this batch, or is the `open` one from an earlier commit, or else is unknown, in which
    /// case the message at `message` stands in.
    fn result(&mut self, result: &ToolResult, open: Option<OpenCall>, message: usize) {
        self.closed.push(result.id.clone());
        let draft = Draft::new(Source::Environment, "tool/result");
        let (draft, tool) = match (self.opened.get(&result.id), open) {
            (Some((call, tool, _)), _) => (draft.cause_in_batch(*call), tool.clone()),
            (None, Some(open)) if open.result.is_some() => return,
            (None, Some(open)) => (draft.cause(open.event), open.tool),
            (None, None) => (draft.cause_in_batch(message), String::new()),
        };
        let payload = result_payload(&result.id, &tool, result.ok, &result.output);
        self.drafts.push(draft.payload(payload.to_string()));
    }
}

impl State {
    const fn new() -> Self {
        Self {
            messages: Vec::new(),
            message_ids: Vec::new(),
            compaction: None,
            input_tokens: 0,
            replied: 0,
            added: 0,
            overflowed: false,
            stopped: None,
            written: 0,
            waits: BTreeMap::new(),
            calls: BTreeMap::new(),
            unfinished: false,
            restarts: 0,
            ended: None,
        }
    }

    /// Note that the open call `call` to `tool` waits for `signal`. It counts as having a
    /// result, the placeholder, so a run picked up after a crash hands that to the model's
    /// turn and waits rather than calling the tool again.
    fn wait(&mut self, call: &str, tool: &str, signal: &str) {
        self.waits.insert(
            call.to_owned(),
            Waited {
                tool: tool.to_owned(),
                event: self.calls.get(call).map(|open| open.event),
                signal: signal.to_owned(),
                payload: None,
            },
        );
        if let Some(open) = self.calls.get_mut(call) {
            open.started = true;
            open.result = Some((true, ToolOutput::text(placeholder(signal))));
        }
    }

    /// Give every call waiting for `name` the signal's `payload`, in place of its placeholder.
    fn resolve(&mut self, name: &str, payload: &str) {
        let mut resolved = false;
        for waited in self.waits.values_mut() {
            if waited.signal == name && waited.payload.is_none() {
                waited.payload = Some(payload.to_owned());
                resolved = true;
            }
        }
        if !resolved {
            return;
        }
        let messages = core::mem::take(&mut self.messages);
        self.messages = messages
            .into_iter()
            .map(|message| self.resolved(message))
            .collect();
    }

    /// `message` with the result of each call whose signal came in place of its placeholder.
    fn resolved(&self, mut message: Message) -> Message {
        for part in &mut message.parts {
            if let Part::ToolResult(result) = part
                && let Some(payload) = self
                    .waits
                    .get(&result.id)
                    .and_then(|waited| waited.payload.as_ref())
            {
                *result = ToolResult::new(result.id.clone(), true, payload.as_str());
            }
        }
        message
    }

    /// Add `message`, from the `message` event `id`, to the conversation.
    fn push(&mut self, id: EventId, message: Message) {
        // A call is open until its response is in the conversation.
        for result in message.tool_results() {
            self.calls.remove(&result.id);
        }
        // A person's message begins a turn, and what the model writes is counted from there.
        if message.role == Role::User && !message.is_tool_results() {
            self.written = 0;
        }
        let message = self.resolved(message);
        self.added = self.added.saturating_add(size(&message));
        self.messages.push(message);
        self.message_ids.push(id);
        self.unfinished = true;
    }

    /// The index of the first message the latest summary does not stand for.
    fn kept_from(&self, keep_from: EventId) -> usize {
        self.message_ids
            .iter()
            .position(|message| *message >= keep_from)
            .unwrap_or(self.message_ids.len())
    }

    /// The session as a step sees it.
    /// Note what the `usage` event `payload` says about a model request: the tokens it read,
    /// and why the model stopped.
    fn used(&mut self, payload: &str) {
        let usage: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
        // Written by this version at the top, by those before it under `usage`.
        self.input_tokens = usage
            .get("input_tokens")
            .or_else(|| usage.pointer("/usage/input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let written = usage
            .get("output_tokens")
            .or_else(|| usage.pointer("/usage/output_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        self.written = self.written.saturating_add(written);
        (self.replied, self.added, self.overflowed) = (written, 0, false);
        self.stopped = usage
            .get("stop")
            .and_then(Value::as_str)
            .and_then(StopReason::parse);
    }

    fn session(&self, id: &SessionId) -> Session {
        let summary = self
            .compaction
            .as_ref()
            .map(|(summary, keep_from)| (summary.clone(), self.kept_from(*keep_from)));
        Session {
            id: id.clone(),
            messages: self.messages.clone(),
            summary,
            stopped: self.stopped,
            written: self.written,
        }
    }

    /// What the next model request is expected to take, in tokens, given the conversation
    /// the model sees: what the last request took and was answered with, and an estimate
    /// of what was added since; or an estimate of all of it, where no request was answered
    /// since the last summary.
    fn expected(&self, view: &[(Option<EventId>, Message)]) -> u64 {
        if self.input_tokens == 0 {
            return estimate(view.iter().map(|(_, message)| size(message)).sum());
        }
        self.input_tokens
            .saturating_add(self.replied)
            .saturating_add(estimate(self.added))
    }

    /// The conversation the model sees, before joining, each message with the event it
    /// came from: `None` for the summary and its acknowledgement.
    fn model_view(&self) -> Vec<(Option<EventId>, Message)> {
        let mut view = Vec::new();
        let from = match &self.compaction {
            Some((summary, keep_from)) => {
                view.push((None, summary_message(summary)));
                view.push((None, Message::assistant().text(SUMMARY_ACK)));
                self.kept_from(*keep_from)
            }
            None => 0,
        };
        for (id, message) in self.message_ids.iter().zip(&self.messages).skip(from) {
            view.push((Some(*id), message.clone()));
        }
        view
    }

    /// Bring the projection up to date with one stored event of the session.
    fn project(&mut self, event: &Event) -> Result<()> {
        // Plugins share a session's stream while they serve its tool calls, but they do not
        // write its history: a plugin's `message`, `gate/answer`, `signal` or `compaction`
        // is its own event, not the conversation's, whatever its topic.
        if matches!(event.source, Source::Plugin(_)) {
            return Ok(());
        }
        match event.topic.as_str() {
            MESSAGE => {
                let message = decode(&event.payload)
                    .with_context(|| format!("event {} is not a message", event.id))?;
                self.push(event.id, message);
            }
            USAGE => self.used(&event.payload),
            COMPACTION => {
                let payload: Value = serde_json::from_str(&event.payload)
                    .with_context(|| format!("event {} is not a compaction", event.id))?;
                let summary = payload
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let keep_from = payload
                    .get("keep_from")
                    .and_then(Value::as_u64)
                    .map_or(event.id, EventId::new);
                self.compaction = Some((summary.to_owned(), keep_from));
                (self.input_tokens, self.overflowed) = (0, false);
            }
            TURN_END => {
                (self.unfinished, self.restarts) = (false, 0);
                self.ended = End::read(&event.payload);
                self.overflowed = self.ended.as_ref().is_some_and(End::overflowed);
            }
            RESTART => self.restarts = self.restarts.saturating_add(1),
            TOOL_WAIT | SIGNAL => {
                let payload: Value = serde_json::from_str(&event.payload)
                    .with_context(|| format!("event {} is not a {}", event.id, event.topic))?;
                let text = |name: &str| {
                    payload
                        .get(name)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                };
                if event.topic == SIGNAL {
                    self.resolve(text("name"), text("payload"));
                } else {
                    self.wait(text("id"), text("tool"), text("signal"));
                }
            }
            "tool/call" | "tool/start" | "tool/result" | GATE_DECISION | GATE_ANSWER => {
                self.project_call(event)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Note what a `tool/call`, `tool/start` or `tool/result` event says about its call.
    fn project_call(&mut self, event: &Event) -> Result<()> {
        let payload: Value = serde_json::from_str(&event.payload)
            .with_context(|| format!("event {} is not about a tool call", event.id))?;
        let text = |name: &str| payload.get(name).and_then(Value::as_str).map(str::to_owned);
        let Some(id) = text("id") else {
            return Ok(());
        };
        if event.topic == "tool/call" {
            let call = OpenCall {
                event: event.id,
                tool: text("tool").unwrap_or_default(),
                input: payload
                    .get("input")
                    .map_or_else(String::new, Value::to_string),
                gated: None,
                started: false,
                result: None,
            };
            self.calls.insert(id, call);
            return Ok(());
        }
        // Read before the call is borrowed: an image that is not base64 is an error.
        let output = if event.topic == "tool/result" {
            Some(recorded_output(event.id, &payload)?)
        } else {
            None
        };
        if let Some(call) = self.calls.get_mut(&id) {
            note(call, &event.topic, &payload, output);
        }
        Ok(())
    }
}

/// Note on the open `call` what the event on `topic` with `payload` says about it:
/// `output` is what a `tool/result` says the tool returned.
fn note(call: &mut OpenCall, topic: &str, payload: &Value, output: Option<ToolOutput>) {
    let text = |name: &str| payload.get(name).and_then(Value::as_str).map(str::to_owned);
    let reason = text("reason").unwrap_or_default();
    {
        match topic {
            GATE_DECISION => {
                let verdict = match text("verdict").as_deref() {
                    Some("allow") => Verdict::Allow,
                    Some("ask") => Verdict::Ask(reason),
                    _ => Verdict::Deny(reason),
                };
                call.gated = Some(Gated {
                    verdict,
                    gate: text("gate").unwrap_or_default(),
                    answer: None,
                });
            }
            GATE_ANSWER => {
                let allow = payload.get("allow").and_then(Value::as_bool) == Some(true);
                let answer = if allow {
                    Answer::Allow
                } else {
                    Answer::Deny(reason)
                };
                if let Some(gated) = &mut call.gated {
                    gated.answer = Some(answer);
                }
            }
            _ => {
                call.started = true;
                if let Some(output) = output {
                    let ok = payload.get("ok").and_then(Value::as_bool) == Some(true);
                    call.result = Some((ok, output));
                }
            }
        }
    }
}

/// Where the answer to the question a gate asked about `call` is kept, if one asked.
fn answer_slot<'a>(state: &'a mut State, call: &str) -> Option<&'a mut Option<Answer>> {
    let gated = state.calls.get_mut(call)?.gated.as_mut()?;
    matches!(gated.verdict, Verdict::Ask(_)).then_some(&mut gated.answer)
}

/// Records a session's effects on the event stream and rebuilds the session from it.
///
/// The effects of one step of the loop become one commit: they are stored together or not
/// at all, and the conversation the next step sees changes only once they are.
pub struct Journal {
    id: SessionId,
    events: Arc<EventStream>,
    state: Lock<State>,
}

impl core::fmt::Debug for Journal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Journal")
            .field("id", &self.id)
            .field("messages", &self.state(|state| state.messages.len()))
            .finish_non_exhaustive()
    }
}

impl Journal {
    /// A journal for the session `id`, picking up whatever conversation `events` already
    /// holds for it. Pass a stream loaded with [`EventStream::load`] to resume an earlier
    /// run.
    ///
    /// # Errors
    ///
    /// Fails if a `message` or tool-call event of the session cannot be decoded.
    pub fn new(id: SessionId, events: Arc<EventStream>) -> Result<Self> {
        let mut state = State::new();
        for event in read_all(&events, Query::first(PAGE).session(id.clone()), |_| true)? {
            state.project(&event)?;
        }
        Ok(Self {
            id,
            events,
            state: Lock::new(state),
        })
    }

    /// The session as it stands.
    #[must_use]
    pub fn session(&self) -> Session {
        self.state(|state| state.session(&self.id))
    }

    /// The session this journal records.
    #[must_use]
    pub const fn id(&self) -> &SessionId {
        &self.id
    }

    /// Append `message` to the conversation and publish it, along with an event for each
    /// tool call and tool result it carries, as one commit.
    ///
    /// # Errors
    ///
    /// Fails if the commit cannot be stored.
    pub fn record(&self, message: &Message) -> Result<EventId> {
        self.record_from(message, None)
    }

    /// As [`record`](Self::record), saying which plugin the message was given through, if
    /// it was not given to the host itself: the event's payload names it under `via`.
    pub(crate) fn record_from(&self, message: &Message, via: Option<&str>) -> Result<EventId> {
        let mut batch = Batch::default();
        let position = self.draft_message(&mut batch, message);
        if let (Some(via), Some(draft)) = (via, batch.drafts.get_mut(position)) {
            let mut payload = encode(message);
            if let Some(fields) = payload.as_object_mut() {
                fields.insert("via".to_owned(), json!(via));
            }
            *draft = Draft::new(Source::User, MESSAGE).payload(payload.to_string());
        }
        let ids = self.commit(batch)?;
        ids.get(position)
            .copied()
            .context("the commit returned no id for the message")
    }

    /// Store the effects of one step as one commit, then bring the projection up to date.
    ///
    /// # Errors
    ///
    /// Fails if the commit cannot be stored.
    pub fn apply(&self, effects: Vec<Effect>) -> Result<()> {
        let mut batch = Batch::default();
        for effect in effects {
            match effect {
                Effect::Message(message) => {
                    self.draft_message(&mut batch, &message);
                }
                Effect::Usage { model, usage, stop } => {
                    let payload = usage_payload(&model, usage, stop);
                    batch
                        .drafts
                        .push(Draft::new(Source::Agent, USAGE).payload(payload.to_string()));
                    batch.asked = Some(Asked {
                        input: usage.input_tokens.unwrap_or(0),
                        output: usage.output_tokens.unwrap_or(0),
                        stopped: stop,
                    });
                }
                Effect::Compacted {
                    summary,
                    keep_from,
                    by,
                } => {
                    let payload =
                        json!({"summary": summary, "keep_from": keep_from.get(), "by": by});
                    batch
                        .compactions
                        .push((batch.drafts.len(), summary, keep_from));
                    batch.drafts.push(
                        Draft::new(Source::Host, COMPACTION)
                            .payload(payload.to_string())
                            .cause(keep_from),
                    );
                }
                Effect::Gated { call, decision } => {
                    let (draft, tool) = self.about_call(&call, Source::Host, GATE_DECISION);
                    let payload = json!({
                        "point": TOOL_CALL,
                        "id": call,
                        "tool": tool,
                        "verdict": decision.verdict.kind(),
                        "reason": decision.verdict.reason(),
                        "gate": decision.gate,
                        "caller": "agent",
                    });
                    batch.drafts.push(draft.payload(payload.to_string()));
                    let gated = Gated {
                        verdict: decision.verdict,
                        gate: decision.gate,
                        answer: None,
                    };
                    batch.gated.push((call, gated));
                }
            }
        }
        if !batch.drafts.is_empty() {
            self.commit(batch)?;
        }
        Ok(())
    }

    /// Add `message` to `batch`, with an event for each tool call and tool result it carries.
    /// Returns the position of the `message` event in the batch.
    fn draft_message(&self, batch: &mut Batch, message: &Message) -> usize {
        let source = match message.role {
            Role::Assistant => Source::Agent,
            Role::User if message.is_tool_results() => Source::Environment,
            Role::User => Source::User,
        };
        let recorded = batch.drafts.len();
        batch
            .drafts
            .push(Draft::new(source, MESSAGE).payload(encode(message).to_string()));
        batch.messages.push((recorded, message.clone()));
        for part in &message.parts {
            match part {
                Part::ToolCall(call) => batch.call(call, recorded),
                Part::ToolResult(result) => {
                    let open = self.state(|state| state.calls.get(&result.id).cloned());
                    batch.result(result, open, recorded);
                }
                _ => {}
            }
        }
        recorded
    }

    /// What a compaction could summarise now. `None` if there is nothing but an earlier
    /// summary, or no turn yet.
    ///
    /// The cut falls where a step begins, so that no tool call is parted from its result.
    /// While a turn is under way and every call of its last step is answered, that is the
    /// last step: the model keeps the calls it just made and their results, and everything
    /// before them, the start of the turn included, gives way to the summary. Otherwise it
    /// is where the turn began, and the turn is kept whole.
    pub(crate) fn compactable(&self) -> Option<Compactable> {
        let (view, expected, overflowed, open) = self.state(|state| {
            let view = state.model_view();
            let open =
                !state.calls.is_empty() || state.waits.values().any(|wait| wait.payload.is_none());
            let expected = state.expected(&view);
            (view, expected, state.overflowed, open)
        });
        // The turn began with the last message a person wrote that the model still sees.
        let kickoff = view.iter().rposition(|(id, message)| {
            id.is_some() && message.role == Role::User && !message.is_tool_results()
        })?;
        let answered = view
            .last()
            .is_some_and(|(_, message)| message.is_tool_results());
        let step = view
            .iter()
            .rposition(|(id, message)| id.is_some() && message.role == Role::Assistant)
            .filter(|step| !open && answered && *step > kickoff);
        let cut = step.unwrap_or(kickoff);
        let earlier = view.get(..cut)?;
        // With nothing but an earlier summary before the cut, there is nothing new.
        let messages = earlier.iter().filter(|(id, _)| id.is_some()).count();
        if messages == 0 {
            return None;
        }
        // A cut inside the turn hides what the person asked for, so it is kept in words.
        let request = step
            .and_then(|_| view.get(kickoff))
            .map(|(_, message)| message.text_content());
        Some(Compactable {
            keep_from: view.get(cut)?.0?,
            input_tokens: expected,
            messages,
            overflowed,
            request,
        })
    }

    /// The messages the model sees from before `keep_from`, as text for a summariser: who
    /// said what, and which tools were called. Built only once a compaction is due.
    pub(crate) fn transcript_before(&self, keep_from: EventId) -> String {
        let view = self.state(|state| state.model_view());
        let earlier: Vec<&Message> = view
            .iter()
            .take_while(|(id, _)| *id != Some(keep_from))
            .map(|(_, message)| message)
            .collect();
        transcript(&earlier)
    }

    /// Whether a run of this session began and has not reached its `turn/end`: it is still
    /// going, or it was interrupted.
    #[must_use]
    pub fn unfinished(&self) -> bool {
        self.state(|state| state.unfinished)
    }

    /// How the last run ended, if it recorded that.
    pub(crate) fn ended(&self) -> Option<End> {
        self.state(|state| state.ended.clone())
    }

    /// How far the log says the unanswered tool call `call` got.
    pub(crate) fn progress(&self, call: &str) -> Progress {
        self.state(|state| match state.calls.get(call) {
            Some(OpenCall {
                result: Some((ok, output)),
                ..
            }) => Progress::Finished {
                ok: *ok,
                output: output.clone(),
            },
            Some(OpenCall { started: true, .. }) => Progress::Started,
            _ => Progress::NotStarted,
        })
    }

    /// What the gates decided about the unanswered tool call `call`, if they were asked.
    pub(crate) fn gated(&self, call: &str) -> Option<Gated> {
        self.state(|state| state.calls.get(call).and_then(|open| open.gated.clone()))
    }

    /// The tool calls a gate asked about that nobody has answered yet, in order of id. While
    /// there are any, the run is waiting and goes no further.
    #[must_use]
    pub fn waiting(&self) -> Vec<Ask> {
        self.state(|state| {
            state
                .calls
                .iter()
                .filter_map(|(call, open)| match &open.gated {
                    Some(Gated {
                        verdict: Verdict::Ask(reason),
                        gate,
                        answer: None,
                    }) => Some(Ask {
                        session: self.id.clone(),
                        call: call.clone(),
                        tool: open.tool.clone(),
                        input: open.input.clone(),
                        reason: reason.clone(),
                        gate: gate.clone(),
                    }),
                    _ => None,
                })
                .collect()
        })
    }

    /// Record a person's answer to the gate that asked about `call`, given through the
    /// plugin `via` if it was not given to the host itself. Returns `false`, and records
    /// nothing, if no gate is waiting for an answer about `call`.
    ///
    /// Two answers to one question may come at once, from a terminal and from a page. The
    /// first takes it: the question is marked answered before the answer is stored, so the
    /// second finds nothing waiting.
    pub(crate) fn answer(&self, call: &str, answer: &Answer, via: Option<&str>) -> Result<bool> {
        let taken = self.state(|state| match answer_slot(state, call) {
            Some(slot @ None) => {
                *slot = Some(answer.clone());
                true
            }
            _ => false,
        });
        if !taken {
            return Ok(false);
        }
        let (draft, tool) = self.about_call(call, Source::User, GATE_ANSWER);
        let (allow, reason) = match answer {
            Answer::Allow => (true, ""),
            Answer::Deny(reason) => (false, reason.as_str()),
        };
        let mut payload = json!({"id": call, "tool": tool, "allow": allow, "reason": reason});
        if let (Some(via), Some(fields)) = (via, payload.as_object_mut()) {
            fields.insert("via".to_owned(), json!(via));
        }
        if let Err(error) = self.events.emit(draft.payload(payload.to_string())) {
            // Not stored, so not answered.
            self.state(|state| answer_slot(state, call).map(Option::take));
            return Err(error.into());
        }
        Ok(true)
    }

    /// Record that the tool behind `call` is about to run. This is its own commit, so that
    /// after a crash the log tells a call that was running from one that never began.
    pub(crate) fn start_call(&self, call: &str) -> Result<()> {
        let (draft, tool) = self.about_call(call, Source::Host, "tool/start");
        self.events
            .emit(draft.payload(json!({"id": call, "tool": tool}).to_string()))?;
        self.state(|state| {
            if let Some(open) = state.calls.get_mut(call) {
                open.started = true;
            }
        });
        Ok(())
    }

    /// Record what the tool behind `call` returned, as soon as it has. This is its own
    /// commit, so a result is not lost with whatever else the step was still doing.
    ///
    /// `output` is what the tool returned, or the text of its error. It is stored whole,
    /// images included, so a run picked up later hands the model the same result.
    pub(crate) fn finish_call(&self, call: &str, ok: bool, output: &ToolOutput) -> Result<()> {
        let (draft, tool) = self.about_call(call, Source::Environment, "tool/result");
        let payload = result_payload(call, &tool, ok, output);
        self.events.emit(draft.payload(payload.to_string()))?;
        self.state(|state| {
            if let Some(open) = state.calls.get_mut(call) {
                open.result = Some((ok, output.clone()));
            }
        });
        Ok(())
    }

    /// Record that the call `call` waits for `signal`. Returns the placeholder that stands
    /// for its result until the signal comes.
    pub(crate) fn wait_call(&self, call: &str, signal: &str) -> Result<String> {
        let (draft, tool) = self.about_call(call, Source::Host, TOOL_WAIT);
        let payload = json!({"id": call, "tool": tool, "signal": signal});
        self.events.emit(draft.payload(payload.to_string()))?;
        self.state(|state| state.wait(call, &tool, signal));
        Ok(placeholder(signal))
    }

    /// Store the signal `name`, bringing `payload` to whichever calls of this session wait
    /// for it. Returns whether any did; if none did, nothing is stored.
    ///
    /// Each call that waited gets its `tool/result` in the same commit, caused by its
    /// `tool/call`, so that every call has one whether or not it waited. The conversation
    /// is still rebuilt from the `signal` alone, as a log written before this holds no
    /// such result.
    ///
    /// # Errors
    ///
    /// Fails if the signal cannot be stored.
    pub fn signal(&self, name: &str, payload: &str) -> Result<bool> {
        let results: Vec<Draft> = self.state(|state| {
            state
                .waits
                .iter()
                .filter(|(_, waited)| waited.signal == name && waited.payload.is_none())
                .map(|(call, waited)| waited.brought(call, payload).session(self.id.clone()))
                .collect()
        });
        if results.is_empty() {
            return Ok(false);
        }
        let draft = Draft::new(Source::Environment, SIGNAL)
            .session(self.id.clone())
            .payload(json!({"name": name, "payload": payload}).to_string());
        self.events.commit(core::iter::once(draft).chain(results))?;
        self.state(|state| state.resolve(name, payload));
        Ok(true)
    }

    /// The tool calls of this session still waiting for their signal, in order of id.
    #[must_use]
    pub fn awaiting(&self) -> Vec<Wait> {
        self.state(|state| {
            state
                .waits
                .iter()
                .filter(|(_, waited)| waited.payload.is_none())
                .map(|(call, waited)| Wait {
                    session: self.id.clone(),
                    call: call.clone(),
                    tool: waited.tool.clone(),
                    signal: waited.signal.clone(),
                })
                .collect()
        })
    }

    /// An event of this session on `topic` caused by the open tool call `call`, and the name
    /// of the call's tool.
    fn about_call(&self, call: &str, source: Source, topic: &str) -> (Draft, String) {
        let draft = Draft::new(source, topic).session(self.id.clone());
        let open = self.state(|state| {
            state
                .calls
                .get(call)
                .map(|open| (open.event, open.tool.clone()))
        });
        match open {
            Some((event, tool)) => (draft.cause(event), tool),
            None => (draft, String::new()),
        }
    }

    /// Record that `request` is about to go to the model `provider` serves. This is its own
    /// commit, so the log says when the model was asked, and that it was, even if no answer
    /// ever comes.
    pub(crate) fn ask_started(&self, provider: &str, request: &ModelRequest) -> Result<()> {
        let payload = json!({
            "provider": provider,
            "model": request.model,
            "messages": request.messages.len(),
            "tools": request.tools.len(),
        });
        let draft = Draft::new(Source::Host, MODEL_REQUEST)
            .session(self.id.clone())
            .payload(payload.to_string());
        self.events.emit(draft)?;
        Ok(())
    }

    /// Record that the request to `model` of `provider` failed and is being made again:
    /// which retry it is, why the attempt failed, how long is waited first, and who retries.
    pub(crate) fn retried(&self, provider: &str, model: &str, retry: &Retry) -> Result<()> {
        let payload = json!({
            "provider": provider,
            "model": model,
            "attempt": retry.attempt,
            "kind": retry.error.kind().name(),
            "reason": retry.error.to_string(),
            "delay_ms": u64::try_from(retry.delay.as_millis()).unwrap_or(u64::MAX),
            "by": retry.by,
        });
        let draft = Draft::new(Source::Host, MODEL_RETRY)
            .session(self.id.clone())
            .payload(payload.to_string());
        self.events.emit(draft)?;
        Ok(())
    }

    /// Record `payload` under `topic` as something the host notes about a request to the
    /// model: a checkpoint of what it is writing, or what it had written when it ended
    /// without an answer. Neither is part of the conversation.
    pub(crate) fn noted(&self, topic: &str, payload: &Value) -> Result<()> {
        let draft = Draft::new(Source::Host, topic)
            .session(self.id.clone())
            .payload(payload.to_string());
        self.events.emit(draft)?;
        Ok(())
    }

    /// Record that a person asked the run to stop.
    pub(crate) fn stop_asked(&self, via: Option<&str>) -> Result<()> {
        let payload = via.map_or_else(|| json!({}), |via| json!({"via": via}));
        let draft = Draft::new(Source::User, STOP)
            .session(self.id.clone())
            .payload(payload.to_string());
        self.events.emit(draft)?;
        Ok(())
    }

    /// Record that the run is over, and how it ended.
    pub(crate) fn end_turn(&self, end: &End) -> Result<()> {
        let draft = Draft::new(Source::Host, TURN_END)
            .session(self.id.clone())
            .payload(end.payload().to_string());
        self.events.emit(draft)?;
        self.state(|state| {
            (state.unfinished, state.restarts) = (false, 0);
            state.overflowed = end.overflowed();
            state.ended = Some(end.clone());
        });
        Ok(())
    }

    /// Record that the unfinished run is being picked up again. Returns how many times that
    /// has now happened.
    pub(crate) fn restart(&self) -> Result<u32> {
        let attempt = self.state(|state| state.restarts.saturating_add(1));
        let draft = Draft::new(Source::Host, RESTART)
            .session(self.id.clone())
            .payload(json!({"attempt": attempt}).to_string());
        self.events.emit(draft)?;
        self.state(|state| state.restarts = attempt);
        Ok(attempt)
    }

    /// Store `batch` as one commit of this session, then bring the projection up to date.
    fn commit(&self, mut batch: Batch) -> Result<Vec<EventId>> {
        let drafts = core::mem::take(&mut batch.drafts)
            .into_iter()
            .map(|draft| draft.session(self.id.clone()));
        let ids = self.events.commit(drafts)?;
        self.state(|state| state.catch_up(&ids, batch));
        Ok(ids)
    }

    /// Run `work` with the projection, holding its lock until it returns.
    fn state<R>(&self, work: impl FnOnce(&mut State) -> R) -> R {
        self.state.with(work)
    }
}

impl State {
    /// Bring the projection up to date with a batch committed as `ids`.
    fn catch_up(&mut self, ids: &[EventId], batch: Batch) {
        for (position, message) in batch.messages {
            if let Some(id) = ids.get(position).copied() {
                self.push(id, message);
            }
        }
        for (position, summary, keep_from) in batch.compactions {
            if ids.get(position).is_some() {
                self.compaction = Some((summary, keep_from));
                (self.input_tokens, self.overflowed) = (0, false);
            }
        }
        for (call, (position, tool, input)) in batch.opened {
            let Some(event) = ids.get(position).copied() else {
                continue;
            };
            let open = OpenCall {
                event,
                tool,
                input,
                gated: None,
                started: false,
                result: None,
            };
            self.calls.insert(call, open);
        }
        for (call, gated) in batch.gated {
            if let Some(open) = self.calls.get_mut(&call) {
                open.gated = Some(gated);
            }
        }
        for call in batch.closed {
            self.calls.remove(&call);
        }
        if let Some(asked) = batch.asked {
            (self.input_tokens, self.stopped) = (asked.input, asked.stopped);
            self.written = self.written.saturating_add(asked.output);
            (self.replied, self.added, self.overflowed) = (asked.output, 0, false);
        }
    }
}

/// What a compaction could summarise. See [`Journal::compactable`].
#[derive(Debug, Clone)]
pub(crate) struct Compactable {
    /// The first message to keep: where the current turn began, or its last step.
    pub(crate) keep_from: EventId,
    /// What the next model request is expected to take, in tokens: the last request's
    /// count and answer, and an estimate of what was added since.
    pub(crate) input_tokens: u64,
    /// How many messages would be summarised, an earlier summary not counted.
    pub(crate) messages: usize,
    /// Whether the last run ended because its request was more than the model holds.
    pub(crate) overflowed: bool,
    /// What the person asked for in the turn under way, if the cut would hide it.
    pub(crate) request: Option<String>,
}

/// The bytes of `message` that a model reads: its text, its tool calls and their results.
fn size(message: &Message) -> usize {
    message
        .parts
        .iter()
        .map(|part| match part {
            Part::Text(text) => text.len(),
            Part::ToolCall(call) => call.name.len().saturating_add(call.input.len()),
            Part::ToolResult(result) => result
                .output
                .content()
                .iter()
                .map(|block| match block {
                    Content::Text(text) => text.len(),
                    _ => 0,
                })
                .sum(),
            _ => 0,
        })
        .sum()
}

/// `bytes` of text as tokens, counted as the core counts them.
fn estimate(bytes: usize) -> u64 {
    u64::try_from(bytes)
        .unwrap_or(u64::MAX)
        .div_ceil(u64::from(BYTES_PER_TOKEN))
}

/// Longest part of a message a transcript keeps, in characters.
const TRANSCRIPT_PART: usize = 2000;

/// `messages` as text for a summariser: who said what, and which tools were called.
fn transcript(messages: &[&Message]) -> String {
    let mut lines = Vec::new();
    for message in messages {
        let role = match message.role {
            Role::User if message.is_tool_results() => "tool",
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        for part in &message.parts {
            let text = match part {
                Part::Text(text) => text.clone(),
                Part::ToolCall(call) => format!("called {} with {}", call.name, call.input),
                Part::ToolResult(result) => result.output.to_text(),
                Part::Image { mime_type, data } => {
                    format!("[image: {mime_type}, {} bytes]", data.len())
                }
                _ => continue,
            };
            if text.trim().is_empty() {
                continue;
            }
            let mut shown: String = text.chars().take(TRANSCRIPT_PART).collect();
            if shown.len() < text.len() {
                shown.push_str(" [cut]");
            }
            lines.push(format!("{role}: {shown}"));
        }
    }
    lines.join("\n")
}

/// The payload of a `usage` event: the model, each count the provider gave, and why the
/// model stopped if the provider said.
fn usage_payload(model: &str, usage: Usage, stop: Option<StopReason>) -> Value {
    let mut payload = serde_json::Map::new();
    payload.insert("model".to_owned(), json!(model));
    payload.extend(
        usage
            .counts()
            .map(|(name, count)| (name.to_owned(), json!(count))),
    );
    if let Some(stop) = stop {
        payload.insert("stop".to_owned(), json!(stop.as_str()));
    }
    Value::Object(payload)
}

/// The payload of the `tool/result` event for the call `id`: the result's text as `output`,
/// and its blocks as `content` unless the result is one block of text, which `output`
/// already is. [`recorded_output`] reads it back.
fn result_payload(id: &str, tool: &str, ok: bool, output: &ToolOutput) -> Value {
    let mut payload = json!({"id": id, "tool": tool, "ok": ok, "output": output.to_text()});
    if !matches!(output.content(), [Content::Text(_)])
        && let Some(fields) = payload.as_object_mut()
    {
        fields.insert("content".to_owned(), encode_content(output));
    }
    payload
}

/// What the `tool/result` event `id`, with `payload`, says the tool returned: its `content`
/// blocks if it has them, or else its `output` as one block of text.
fn recorded_output(id: EventId, payload: &Value) -> Result<ToolOutput> {
    let Some(blocks) = payload.get("content").and_then(Value::as_array) else {
        let output = payload.get("output").and_then(Value::as_str);
        return Ok(ToolOutput::text(output.unwrap_or_default()));
    };
    decode_content(blocks).with_context(|| format!("event {id} has an image that is not base64"))
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

    /// `output` written as a `tool/result` and read back.
    fn round_trip(output: &ToolOutput) -> ToolOutput {
        let payload = result_payload("c1", "snap", true, output);
        recorded_output(EventId::new(1), &payload).unwrap()
    }

    #[test]
    fn usage_records_only_the_counts_the_provider_gave() {
        let plain = usage_payload("m", Usage::new(Some(10), None), None);
        assert_eq!(plain, json!({"model": "m", "input_tokens": 10}));
        let cached = Usage::new(Some(10), Some(2))
            .with_cache(Some(8), None)
            .with_reasoning(Some(1));
        assert_eq!(
            usage_payload("m", cached, Some(StopReason::Limit)),
            json!({
                "model": "m", "input_tokens": 10, "output_tokens": 2, "cache_read_tokens": 8,
                "reasoning_tokens": 1, "stop": "limit"
            })
        );
    }

    #[test]
    fn a_recorded_result_reads_back_as_the_tool_returned_it() {
        let outputs = [
            ToolOutput::text("plain"),
            ToolOutput::text(""),
            ToolOutput::default(),
            ToolOutput::new(vec![Content::text("one"), Content::text("two")]),
            ToolOutput::from(Content::image("image/png", PNG)),
            ToolOutput::new(vec![
                Content::text("before"),
                Content::image("image/png", PNG),
                Content::image("image/jpeg", Vec::new()),
                Content::text("after"),
            ]),
        ];
        for output in outputs {
            assert_eq!(round_trip(&output), output);
        }
    }

    #[test]
    fn one_block_of_text_is_recorded_as_output_alone() {
        assert_eq!(
            result_payload("c1", "label", true, &ToolOutput::text("a view")).to_string(),
            r#"{"id":"c1","tool":"label","ok":true,"output":"a view"}"#
        );
        assert_eq!(
            result_payload(
                "c1",
                "snap",
                false,
                &Content::image("image/png", PNG).into()
            ),
            json!({
                "id": "c1",
                "tool": "snap",
                "ok": false,
                "output": "[image: image/png, 8 bytes]",
                "content": [{"type": "image", "mime_type": "image/png", "data": "iVBORw0KGgo="}],
            })
        );
    }

    #[test]
    fn a_record_this_version_did_not_write_is_read_as_far_as_it_can_be() {
        let read = |payload: Value| recorded_output(EventId::new(7), &payload);
        // A log from before results had blocks: the output is the result.
        assert_eq!(
            read(json!({"id": "c1", "ok": true, "output": "old"})).unwrap(),
            ToolOutput::text("old")
        );
        // A kind of block from a later version is noted, not dropped; the rest is kept.
        let later =
            json!({"content": [{"type": "audio", "data": "AAAA"}, {"type": "text", "text": "x"}]});
        assert_eq!(
            read(later).unwrap().content(),
            [Content::text("[audio content]"), Content::text("x")]
        );
        // An image that cannot be decoded is not passed off as one.
        let broken = json!({"content": [{"type": "image", "mime_type": "image/png", "data": "!"}]});
        assert_eq!(
            read(broken).unwrap_err().to_string(),
            "event #7 has an image that is not base64"
        );
    }

    #[test]
    fn a_run_says_how_it_ended() {
        for end in [
            End::Finished,
            End::TurnLimit,
            End::Failed("the model is down".into()),
            End::ModelFailed(
                ProviderError::new("too much to read").of(ProviderErrorKind::ContextOverflow),
            ),
            End::ModelFailed(ProviderError::new("who knows")),
            End::Stopped,
            End::Yielded,
        ] {
            assert_eq!(End::read(&end.payload().to_string()), Some(end));
        }
        // A model's failure is a failed run to whoever reads the status alone, and a kind
        // a later harness wrote is read as no particular one.
        let overflow =
            End::ModelFailed(ProviderError::new("too much").of(ProviderErrorKind::ContextOverflow));
        assert_eq!(
            overflow.payload(),
            json!({"status": "failed", "reason": "too much", "kind": "context-overflow"})
        );
        let later = json!({"status": "failed", "reason": "new", "kind": "not-yet-known"});
        assert_eq!(
            End::read(&later.to_string()),
            Some(End::ModelFailed(ProviderError::new("new")))
        );
        // A `turn/end` written before runs said how they ended.
        assert_eq!(End::read(""), None);
    }

    #[test]
    fn the_model_sees_a_summary_and_alternating_roles() {
        let session = Session {
            id: SessionId::new("main"),
            messages: vec![
                Message::user().text("old question"),
                Message::assistant().text("old answer"),
                Message::user().text("new question"),
                Message::user().text("and another thing"),
                Message::assistant().call(ToolCall::new("1", "read", "{}")),
                Message::user().result(ToolResult::new("1", true, "text")),
            ],
            summary: Some(("they talked".into(), 2)),
            stopped: None,
            written: 0,
        };

        let view = session.for_model();
        let roles: Vec<Role> = view.iter().map(|message| message.role).collect();
        assert_eq!(
            roles,
            [
                Role::User,
                Role::Assistant,
                Role::User,
                Role::Assistant,
                Role::User
            ]
        );
        assert!(view[0].text_content().contains("they talked"));
        assert_eq!(view[2].text_content(), "new question\nand another thing");
        assert_eq!(session.turn().len(), 3);
        assert_eq!(session.model_turns(), 1);
        assert_eq!(session.pending_calls(), Vec::<ToolCall>::new());
        assert_eq!(session.answer(), None);
    }
}
