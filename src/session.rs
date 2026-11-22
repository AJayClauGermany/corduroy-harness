//! The conversation as a projection of the event stream.
//!
//! oi-kit's state machine never mutates a conversation. Its operations return [`Effect`]s,
//! and the host decides what persisting them means. Here it means appending to the event
//! stream: the [`Journal`] turns each effect into events, and the conversation a model sees
//! is whatever the `message` events add up to. A run can therefore be resumed, replayed or
//! inspected from the log alone, and every plugin sees the same history the model does.
//!
//! One stream holds many sessions. Each begins with a `session/start` event, and every event
//! below names the session it belongs to, so a session is rebuilt from its own events alone.
//!
//! | Topic | Source | Payload |
//! | --- | --- | --- |
//! | `session/start` | `host` | `{"parent"}`, or `{}` for a session with no parent |
//! | `message` | `user`, `agent` or `environment` | the oi-kit message, as JSON |
//! | `tool/call` | `agent` | `{"id", "tool", "input"}`, caused by its `message` |
//! | `gate/decision` | `host` | `{"point", "id", "tool", "verdict", "reason", "gate", "caller"}`, caused by its `tool/call`: what the gates said |
//! | `gate/answer` | `user` | `{"id", "tool", "allow", "reason"}`, caused by its `tool/call`: a person's answer to a gate that asked |
//! | `tool/start` | `host` | `{"id", "tool"}`, caused by its `tool/call`: the tool is about to run |
//! | `tool/result` | `environment` | `{"id", "tool", "ok", "output"}`, and `"content"` unless the result is one block of text, caused by its `tool/call` |
//! | `usage` | `agent` | the provider's usage report, as JSON |
//! | `turn/end` | `host` | empty |
//! | `run/restart` | `host` | `{"attempt"}`: an unfinished run was picked up again |
//! | `tool/wait` | `host` | `{"id", "tool", "signal"}`, caused by its `tool/call`: the call's result comes with the signal |
//! | `signal` | `environment` | `{"name", "payload"}`: what a waiting call was waiting for |
//! | `compaction` | `host` | `{"summary", "keep_from", "by"}`: the messages before the `message` event `keep_from` are summarised |
//!
//! A tool call leaves a trail of its own, one commit at a time: `tool/call` with the model's
//! message, `tool/start` just before the tool runs, `tool/result` as soon as it returns, and
//! only then the `message` that carries the results of every call of that step to the
//! model. After a crash the log therefore says of each call whether it never started, was
//! running, or finished with a result that only needs handing over.
//!
//! A tool's result is a list of content blocks (see [`ToolOutput`]). Its `tool/result` always
//! has the result's text as `output`: the text blocks joined by newlines, with an image
//! standing as a marker such as `[image: image/png, 1234 bytes]`. For a result that is one
//! block of text, which most are, that is all of it. Any other result also has its blocks as
//! `content`, an array of `{"type": "text", "text"}` and
//! `{"type": "image", "mime_type", "data"}` with the image's bytes in base64, so that the
//! log alone gives back the result the tool returned, images included.
//!
//! Where a gate guards tool calls, its decision on each call is stored before the tool can
//! start. A call a gate asked about waits for a `gate/answer`.
//!
//! A run is *unfinished* from its first `message` until its `turn/end`. A session whose
//! events stop short of that was interrupted, and [`Harness::continue_run`] picks it up,
//! unless it is *waiting*: a gate asked about one of its calls and nobody has answered yet.
//! [`Harness::answer`] gives the answer and takes the run on.
//!
//! A tool call may *wait*: whatever served it said, through
//! [`corduroy_core::call::wait_for`], that its result will come with a signal. The call
//! gets a placeholder result and a `tool/wait`, and the run stops before the model is asked
//! again; when a `signal` of that name is stored, its payload takes the placeholder's place
//! and the run can go on. A child session's end is such a signal (`session/<id>/end`), so a
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

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use corduroy_core::{
    Content, Decision, Draft, Event, EventId, EventStream, LogError, Pattern, SessionId, Source,
    TOOL_CALL, ToolOutput, Verdict,
};

use crate::harness::HarnessError;
use oi_kit_agent::inference::InferenceEffect;
use oi_kit_agent::machine::{EffectHandler, MachineSession, SessionLoader};
use oi_kit_agent::operation::{Emitter, MachineEffect};
use oi_kit_provider_types::conversation::Conversation;
use oi_kit_provider_types::conversation::message::{
    Message, MessageContent, ToolRequest, ToolResponse,
};
use oi_kit_provider_types::conversation::token_usage::ProviderUsage;
use rmcp::model::{CallToolResult, ContentBlock, Role};
use serde_json::{Value, json};

/// Topic of the events that make up the conversation.
pub const MESSAGE: &str = "message";

/// Topic of the event that ends a run.
const TURN_END: &str = "turn/end";

/// Topic of the event that reports what a model request cost.
const USAGE: &str = "usage";

/// Topic of the event that records an unfinished run being picked up again.
const RESTART: &str = "run/restart";

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

/// Topic of the event that stands a summary in for the messages before a point.
pub const COMPACTION: &str = "compaction";

/// What the model is told about a summary that replaced the start of the conversation.
const SUMMARY_INTRO: &str = "The start of this conversation was condensed to save space. This \
                             summary takes its place:";

/// The model's side of the summary, so the conversation still alternates.
const SUMMARY_ACK: &str = "Understood. I will continue from that summary.";

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

    /// Every session `events` holds, in the order they were started.
    pub(crate) fn all(events: &EventStream) -> Vec<Self> {
        events
            .matching(&Pattern::new(SESSION_START))
            .into_iter()
            // Only the harness starts sessions; a plugin's `session/start` is not one.
            .filter(|event| event.source == Source::Host)
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
            .collect()
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
    known: Mutex<BTreeMap<SessionId, Known>>,
}

impl std::fmt::Debug for Sessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
        let mut known: BTreeMap<SessionId, Known> = SessionInfo::all(&events)
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
            known: Mutex::new(known),
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
        let mut known = self.known();
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
        drop(known);
        Ok(id)
    }

    /// Every session, in order of id.
    #[must_use]
    pub fn list(&self) -> Vec<SessionInfo> {
        self.known()
            .values()
            .map(|known| known.info.clone())
            .collect()
    }

    /// The journal of `session`, rebuilt from the stream the first time it is asked for.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::UnknownSession`] if there is no such session, or
    /// [`HarnessError::Run`] if its events cannot be decoded.
    pub fn journal(&self, session: &SessionId) -> Result<Arc<Journal>, HarnessError> {
        let mut known = self.known();
        let entry = known
            .get_mut(session)
            .ok_or_else(|| HarnessError::UnknownSession(session.clone()))?;
        if let Some(journal) = &entry.journal {
            return Ok(Arc::clone(journal));
        }
        let journal = Arc::new(Journal::new(session.clone(), Arc::clone(&self.events))?);
        entry.journal = Some(Arc::clone(&journal));
        drop(known);
        Ok(journal)
    }

    fn known(&self) -> MutexGuard<'_, BTreeMap<SessionId, Known>> {
        self.known.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One conversation, as the state machine sees it at the start of a step.
#[derive(Debug, Clone)]
pub struct Session {
    id: String,
    conversation: Conversation,
}

impl Session {
    /// The messages so far.
    #[must_use]
    pub const fn conversation(&self) -> &Conversation {
        &self.conversation
    }
}

impl MachineSession for Session {
    fn id(&self) -> &str {
        &self.id
    }

    fn conversation(&self) -> Option<&Conversation> {
        Some(&self.conversation)
    }
}

/// Something an operation wants recorded.
#[derive(Debug)]
#[non_exhaustive]
pub enum Effect {
    /// Append a message to the conversation.
    Message(Box<Message>),
    /// Note what a model request cost.
    Usage(Box<ProviderUsage>),
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

impl From<Message> for Effect {
    fn from(message: Message) -> Self {
        Self::Message(Box::new(message))
    }
}

impl InferenceEffect for Effect {
    fn record_usage(usage: ProviderUsage) -> Self {
        Self::Usage(Box::new(usage))
    }
}

impl MachineEffect for Effect {
    fn ensure_message_ids(&mut self) {
        if let Self::Message(message) = self
            && message.id.is_none()
        {
            **message = message.as_ref().clone().with_generated_id_if_missing();
        }
    }
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
        /// What the tool returned, or the text of its error.
        output: ToolOutput,
    },
}

struct State {
    conversation: Conversation,
    /// The event each message of `conversation` came from, in the same order. A summary
    /// and its acknowledgement have their `compaction` event's id.
    message_ids: Vec<EventId>,
    /// The input tokens of the last model request, unless a compaction came after it.
    input_tokens: u64,
    /// Tool calls that waited for a signal, by tool-call id, with the payload once it came.
    waits: BTreeMap<String, Waited>,
    /// The tool calls waiting for a response, by tool-call id.
    calls: BTreeMap<String, OpenCall>,
    /// Whether a run has begun and not reached its `turn/end`.
    unfinished: bool,
    /// How many times the unfinished run has been picked up again.
    restarts: u32,
}

/// A tool call that waited for a signal.
struct Waited {
    tool: String,
    signal: String,
    /// The signal's payload, once it came.
    payload: Option<String>,
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
    /// The input tokens of a model request recorded in this batch.
    input_tokens: Option<u64>,
}

impl Batch {
    /// Add the `tool/call` event for `request`, caused by the message at `message`.
    fn call(&mut self, request: &ToolRequest, message: usize) {
        let (tool, input) = request.tool_call.as_ref().map_or_else(
            |error| ("<malformed>".to_owned(), json!({"error": error.message})),
            |call| (call.name.to_string(), json!(call.arguments)),
        );
        let payload = json!({"id": request.id, "tool": tool, "input": input});
        self.opened.insert(
            request.id.clone(),
            (self.drafts.len(), tool, input.to_string()),
        );
        self.drafts.push(
            Draft::new(Source::Agent, "tool/call")
                .payload(payload.to_string())
                .cause_in_batch(message),
        );
    }

    /// Add the `tool/result` event for `response`, unless the `open` call it answers already
    /// has one from when the tool returned. The event is caused by its call, which is in
    /// this batch, or is the `open` one from an earlier commit, or else is unknown, in which
    /// case the message at `message` stands in.
    fn result(&mut self, response: &ToolResponse, open: Option<OpenCall>, message: usize) {
        self.closed.push(response.id.clone());
        let draft = Draft::new(Source::Environment, "tool/result");
        let (draft, tool) = match (self.opened.get(&response.id), open) {
            (Some((call, tool, _)), _) => (draft.cause_in_batch(*call), tool.clone()),
            (None, Some(open)) if open.result.is_some() => return,
            (None, Some(open)) => (draft.cause(open.event), open.tool),
            (None, None) => (draft.cause_in_batch(message), String::new()),
        };
        let (ok, output) = match &response.tool_result {
            Ok(result) => (result.is_error != Some(true), output_of(result)),
            Err(error) => (false, ToolOutput::text(&*error.message)),
        };
        let payload = result_payload(&response.id, &tool, ok, &output);
        self.drafts.push(draft.payload(payload.to_string()));
    }
}

impl State {
    /// Note that the open call `call` to `tool` waits for `signal`. It counts as having a
    /// result, the placeholder, so a run picked up after a crash hands that to the model's
    /// turn and waits rather than calling the tool again.
    fn wait(&mut self, call: &str, tool: &str, signal: &str) {
        self.waits.insert(
            call.to_owned(),
            Waited {
                tool: tool.to_owned(),
                signal: signal.to_owned(),
                payload: None,
            },
        );
        if let Some(open) = self.calls.get_mut(call) {
            open.started = true;
            open.result = Some((true, ToolOutput::text(placeholder(signal))));
        }
    }

    fn project_signal(&mut self, event: &Event) -> Result<()> {
        let payload: Value = serde_json::from_str(&event.payload)
            .with_context(|| format!("event {} is not a signal", event.id))?;
        let text = |name: &str| {
            payload
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or_default()
        };
        self.resolve(text("name"), text("payload"));
        Ok(())
    }

    fn project_compaction(&mut self, event: &Event) -> Result<()> {
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
        self.compact(event.id, summary, keep_from);
        Ok(())
    }

    fn project_wait(&mut self, event: &Event) -> Result<()> {
        let payload: Value = serde_json::from_str(&event.payload)
            .with_context(|| format!("event {} is not a tool/wait", event.id))?;
        let text = |name: &str| {
            payload
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or_default()
        };
        self.wait(text("id"), text("tool"), text("signal"));
        Ok(())
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
        let messages = std::mem::take(self.conversation.messages_mut());
        let messages = messages
            .into_iter()
            .map(|message| self.resolved(message))
            .collect();
        *self.conversation.messages_mut() = messages;
    }

    /// `message` with the result of each call whose signal came in place of its placeholder.
    fn resolved(&self, mut message: Message) -> Message {
        for content in &mut message.content {
            if let MessageContent::ToolResponse(response) = content
                && let Some(payload) = self
                    .waits
                    .get(&response.id)
                    .and_then(|waited| waited.payload.as_ref())
            {
                response.tool_result = Ok(CallToolResult::success(vec![ContentBlock::text(
                    payload.clone(),
                )]));
            }
        }
        message
    }

    /// Hide the messages before `keep_from` from the model and stand `summary`, from the
    /// `compaction` event `id`, in for them.
    fn compact(&mut self, id: EventId, summary: &str, keep_from: EventId) {
        let at = self
            .message_ids
            .iter()
            .position(|message| *message >= keep_from)
            .unwrap_or(self.message_ids.len());
        let messages = self.conversation.messages_mut();
        for message in messages.iter_mut().take(at) {
            let user_visible = message.metadata.user_visible;
            *message = message.clone().with_visibility(user_visible, false);
        }
        let summary = Message::user()
            .with_text(format!("{SUMMARY_INTRO}\n<summary>\n{summary}\n</summary>"))
            .agent_only();
        let ack = Message::assistant().with_text(SUMMARY_ACK).agent_only();
        messages.splice(at..at, [summary, ack]);
        self.message_ids.splice(at..at, [id, id]);
        self.input_tokens = 0;
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
                let message: Message = serde_json::from_str(&event.payload)
                    .with_context(|| format!("event {} is not a message", event.id))?;
                // A call is open until its response is in the conversation.
                let answered = message.content.iter().filter_map(|content| match content {
                    MessageContent::ToolResponse(response) => Some(&response.id),
                    _ => None,
                });
                for call in answered {
                    self.calls.remove(call);
                }
                let message = self.resolved(message);
                self.conversation.push(message);
                self.message_ids.push(event.id);
                self.unfinished = true;
            }
            USAGE => {
                let usage: Value = serde_json::from_str(&event.payload).unwrap_or(Value::Null);
                self.input_tokens = usage
                    .pointer("/usage/input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
            }
            COMPACTION => self.project_compaction(event)?,
            TURN_END => (self.unfinished, self.restarts) = (false, 0),
            RESTART => self.restarts = self.restarts.saturating_add(1),
            TOOL_WAIT => self.project_wait(event)?,
            SIGNAL => self.project_signal(event)?,
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
        let Some(call) = self.calls.get_mut(&id) else {
            return Ok(());
        };
        let reason = text("reason").unwrap_or_default();
        match event.topic.as_str() {
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
            topic => {
                call.started = true;
                if topic == "tool/result" {
                    let ok = payload.get("ok").and_then(Value::as_bool) == Some(true);
                    call.result = Some((ok, recorded_output(event.id, &payload)?));
                }
            }
        }
        Ok(())
    }
}

/// Records a session's effects on the event stream and rebuilds the session from it.
///
/// The effects of one step of the loop become one commit: they are stored together or not
/// at all, and the conversation the next step sees changes only once they are.
pub struct Journal {
    id: SessionId,
    events: Arc<EventStream>,
    state: Mutex<State>,
}

impl std::fmt::Debug for Journal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Journal")
            .field("id", &self.id)
            .field("messages", &self.state().conversation.len())
            .finish_non_exhaustive()
    }
}

impl Journal {
    /// A journal for the session `id`, picking up whatever conversation `events` already
    /// holds for it. Pass a stream rebuilt with [`EventStream::replay`] to resume an earlier
    /// run.
    ///
    /// # Errors
    ///
    /// Fails if a `message` or tool-call event of the session cannot be decoded.
    pub fn new(id: SessionId, events: Arc<EventStream>) -> Result<Self> {
        let mut state = State {
            conversation: Conversation::empty(),
            message_ids: Vec::new(),
            input_tokens: 0,
            waits: BTreeMap::new(),
            calls: BTreeMap::new(),
            unfinished: false,
            restarts: 0,
        };
        for event in events.session(&id) {
            state.project(&event)?;
        }
        Ok(Self {
            id,
            events,
            state: Mutex::new(state),
        })
    }

    /// The session as it stands.
    #[must_use]
    pub fn session(&self) -> Session {
        Session {
            id: self.id.as_str().to_owned(),
            conversation: self.state().conversation.clone(),
        }
    }

    /// Append `message` to the conversation and publish it, along with an event for each
    /// tool call and tool result it carries, as one commit.
    ///
    /// # Errors
    ///
    /// Fails if the message cannot be encoded as JSON or the commit cannot be stored.
    pub fn record(&self, message: &Message) -> Result<EventId> {
        let mut batch = Batch::default();
        let position = self.draft_message(&mut batch, message)?;
        let ids = self.commit(batch)?;
        ids.get(position)
            .copied()
            .context("the commit returned no id for the message")
    }

    /// Add `message` to `batch`, with an event for each tool call and tool result it carries.
    /// Returns the position of the `message` event in the batch.
    fn draft_message(&self, batch: &mut Batch, message: &Message) -> Result<usize> {
        let payload = serde_json::to_string(message).context("message is not valid JSON")?;
        let source = match message.role {
            Role::Assistant => Source::Agent,
            Role::User if message.is_tool_response() => Source::Environment,
            Role::User => Source::User,
        };
        let recorded = batch.drafts.len();
        batch
            .drafts
            .push(Draft::new(source, MESSAGE).payload(payload));
        batch.messages.push((recorded, message.clone()));
        for content in &message.content {
            match content {
                MessageContent::ToolRequest(request) => batch.call(request, recorded),
                MessageContent::ToolResponse(response) => {
                    let open = self.state().calls.get(&response.id).cloned();
                    batch.result(response, open, recorded);
                }
                _ => {}
            }
        }
        Ok(recorded)
    }

    /// What a compaction could summarise now: the messages the model sees from before the
    /// current turn began. `None` if there is nothing but an earlier summary, or no turn yet.
    pub(crate) fn compactable(&self) -> Option<Compactable> {
        let state = self.state();
        let messages = state.conversation.messages();
        // The turn began with the last message a person wrote that the model still sees.
        let kickoff = messages.iter().rposition(|message| {
            message.role == Role::User
                && !message.is_tool_response()
                && message.metadata.agent_visible
                && message.metadata.user_visible
        })?;
        let earlier: Vec<&Message> = messages
            .get(..kickoff)?
            .iter()
            .filter(|message| message.metadata.agent_visible)
            .collect();
        // A summary is only for the model; with nothing a person saw, there is nothing new.
        if !earlier.iter().any(|message| message.metadata.user_visible) {
            return None;
        }
        Some(Compactable {
            keep_from: *state.message_ids.get(kickoff)?,
            input_tokens: state.input_tokens,
            messages: earlier.len(),
        })
    }

    /// The messages the model sees from before `keep_from`, as text for a summariser: who
    /// said what, and which tools were called. Built only once a compaction is due.
    pub(crate) fn transcript_before(&self, keep_from: EventId) -> String {
        let state = self.state();
        let at = state
            .message_ids
            .iter()
            .position(|id| *id == keep_from)
            .unwrap_or(0);
        let earlier: Vec<&Message> = state
            .conversation
            .messages()
            .iter()
            .take(at)
            .filter(|message| message.metadata.agent_visible)
            .collect();
        let text = transcript(&earlier);
        drop(earlier);
        drop(state);
        text
    }

    /// The session this journal records.
    #[must_use]
    pub const fn id(&self) -> &SessionId {
        &self.id
    }

    /// Whether a run of this session began and has not reached its `turn/end`: it is still
    /// going, or it was interrupted.
    #[must_use]
    pub fn unfinished(&self) -> bool {
        self.state().unfinished
    }

    /// How far the log says the unanswered tool call `call` got.
    pub(crate) fn progress(&self, call: &str) -> Progress {
        match self.state().calls.get(call) {
            Some(OpenCall {
                result: Some((ok, output)),
                ..
            }) => Progress::Finished {
                ok: *ok,
                output: output.clone(),
            },
            Some(OpenCall { started: true, .. }) => Progress::Started,
            _ => Progress::NotStarted,
        }
    }

    /// What the gates decided about the unanswered tool call `call`, if they were asked.
    pub(crate) fn gated(&self, call: &str) -> Option<Gated> {
        self.state()
            .calls
            .get(call)
            .and_then(|open| open.gated.clone())
    }

    /// The tool calls a gate asked about that nobody has answered yet, in order of id. While
    /// there are any, the run is waiting and goes no further.
    #[must_use]
    pub fn waiting(&self) -> Vec<Ask> {
        self.state()
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
    }

    /// Record a person's answer to the gate that asked about `call`. Returns `false`, and
    /// records nothing, if no gate is waiting for an answer about `call`.
    pub(crate) fn answer(&self, call: &str, answer: &Answer) -> Result<bool> {
        if !self.waiting().iter().any(|ask| ask.call == call) {
            return Ok(false);
        }
        let (draft, tool) = self.about_call(call, Source::User, GATE_ANSWER);
        let (allow, reason) = match answer {
            Answer::Allow => (true, ""),
            Answer::Deny(reason) => (false, reason.as_str()),
        };
        let payload = json!({"id": call, "tool": tool, "allow": allow, "reason": reason});
        self.events.emit(draft.payload(payload.to_string()))?;
        if let Some(gated) = self
            .state()
            .calls
            .get_mut(call)
            .and_then(|open| open.gated.as_mut())
        {
            gated.answer = Some(answer.clone());
        }
        Ok(true)
    }

    /// Record that the tool behind `call` is about to run. This is its own commit, so that
    /// after a crash the log tells a call that was running from one that never began.
    pub(crate) fn start_call(&self, call: &str) -> Result<()> {
        let (draft, tool) = self.about_call(call, Source::Host, "tool/start");
        self.events
            .emit(draft.payload(json!({"id": call, "tool": tool}).to_string()))?;
        if let Some(open) = self.state().calls.get_mut(call) {
            open.started = true;
        }
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
        if let Some(open) = self.state().calls.get_mut(call) {
            open.result = Some((ok, output.clone()));
        }
        Ok(())
    }

    /// Record that the call `call` waits for `signal`. Returns the placeholder that stands
    /// for its result until the signal comes.
    pub(crate) fn wait_call(&self, call: &str, signal: &str) -> Result<String> {
        let (draft, tool) = self.about_call(call, Source::Host, TOOL_WAIT);
        let payload = json!({"id": call, "tool": tool, "signal": signal});
        self.events.emit(draft.payload(payload.to_string()))?;
        self.state().wait(call, &tool, signal);
        Ok(placeholder(signal))
    }

    /// Store the signal `name`, bringing `payload` to whichever calls of this session wait
    /// for it. Returns whether any did; if none did, nothing is stored.
    ///
    /// # Errors
    ///
    /// Fails if the signal cannot be stored.
    pub fn signal(&self, name: &str, payload: &str) -> Result<bool> {
        let awaited = self.awaiting().iter().any(|wait| wait.signal == name);
        if !awaited {
            return Ok(false);
        }
        let draft = Draft::new(Source::Environment, SIGNAL)
            .session(self.id.clone())
            .payload(json!({"name": name, "payload": payload}).to_string());
        self.events.emit(draft)?;
        self.state().resolve(name, payload);
        Ok(true)
    }

    /// The tool calls of this session still waiting for their signal, in order of id.
    #[must_use]
    pub fn awaiting(&self) -> Vec<Wait> {
        self.state()
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
    }

    /// An event of this session on `topic` caused by the open tool call `call`, and the name
    /// of the call's tool.
    fn about_call(&self, call: &str, source: Source, topic: &str) -> (Draft, String) {
        let draft = Draft::new(source, topic).session(self.id.clone());
        match self.state().calls.get(call) {
            Some(open) => (draft.cause(open.event), open.tool.clone()),
            None => (draft, String::new()),
        }
    }

    /// Record that the run is over.
    pub(crate) fn end_turn(&self) -> Result<()> {
        self.events
            .emit(Draft::new(Source::Host, TURN_END).session(self.id.clone()))?;
        let mut state = self.state();
        (state.unfinished, state.restarts) = (false, 0);
        drop(state);
        Ok(())
    }

    /// Record that the unfinished run is being picked up again. Returns how many times that
    /// has now happened.
    pub(crate) fn restart(&self) -> Result<u32> {
        let attempt = self.state().restarts.saturating_add(1);
        let draft = Draft::new(Source::Host, RESTART)
            .session(self.id.clone())
            .payload(json!({"attempt": attempt}).to_string());
        self.events.emit(draft)?;
        self.state().restarts = attempt;
        Ok(attempt)
    }

    /// Store `batch` as one commit of this session, then bring the projection up to date.
    fn commit(&self, batch: Batch) -> Result<Vec<EventId>> {
        let drafts = batch
            .drafts
            .into_iter()
            .map(|draft| draft.session(self.id.clone()));
        let ids = self.events.commit(drafts)?;
        let mut state = self.state();
        for (position, message) in batch.messages {
            let Some(id) = ids.get(position).copied() else {
                continue;
            };
            let message = state.resolved(message);
            state.conversation.push(message);
            state.message_ids.push(id);
            state.unfinished = true;
        }
        for (position, summary, keep_from) in batch.compactions {
            if let Some(id) = ids.get(position).copied() {
                state.compact(id, &summary, keep_from);
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
            state.calls.insert(call, open);
        }
        for (call, gated) in batch.gated {
            if let Some(open) = state.calls.get_mut(&call) {
                open.gated = Some(gated);
            }
        }
        for call in batch.closed {
            state.calls.remove(&call);
        }
        if let Some(tokens) = batch.input_tokens {
            state.input_tokens = tokens;
        }
        drop(state);
        Ok(ids)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What a compaction could summarise. See [`Journal::compactable`].
#[derive(Debug, Clone)]
pub(crate) struct Compactable {
    /// The first message to keep: where the current turn began.
    pub(crate) keep_from: EventId,
    /// The input tokens of the last model request since the last compaction, or zero.
    pub(crate) input_tokens: u64,
    /// How many messages would be summarised.
    pub(crate) messages: usize,
}

/// Longest part of a message a transcript keeps, in characters.
const TRANSCRIPT_PART: usize = 2000;

/// `messages` as text for a summariser: who said what, and which tools were called.
fn transcript(messages: &[&Message]) -> String {
    let mut lines = Vec::new();
    for message in messages {
        let role = match message.role {
            Role::User if message.is_tool_response() => "tool",
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        for content in &message.content {
            let part = content.to_string();
            if part.trim().is_empty() {
                continue;
            }
            let mut shown: String = part.chars().take(TRANSCRIPT_PART).collect();
            if shown.len() < part.len() {
                shown.push_str(" [cut]");
            }
            lines.push(format!("{role}: {shown}"));
        }
    }
    lines.join("\n")
}

/// The blocks of a tool's result as the model is sent them: an image's bytes in base64.
pub(crate) fn blocks_of(output: &ToolOutput) -> Vec<ContentBlock> {
    let block = |block: &Content| match block {
        Content::Image { mime_type, data } => ContentBlock::image(BASE64.encode(data), mime_type),
        text => ContentBlock::text(text.to_string()),
    };
    output.content().iter().map(block).collect()
}

/// The text and image blocks of a tool result in the conversation, as a tool returns them.
fn output_of(result: &CallToolResult) -> ToolOutput {
    let block = |block: &ContentBlock| match block {
        ContentBlock::Text(text) => Some(Content::text(&text.text)),
        ContentBlock::Image(image) => Some(BASE64.decode(&image.data).map_or_else(
            |_| Content::text(format!("[image: {}, not base64]", image.mime_type)),
            |data| Content::image(&image.mime_type, data),
        )),
        _ => None,
    };
    result.content.iter().filter_map(block).collect()
}

/// The payload of the `tool/result` event for the call `id`: the result's text as `output`,
/// and its blocks as `content` unless the result is one block of text, which `output`
/// already is. [`recorded_output`] reads it back.
fn result_payload(id: &str, tool: &str, ok: bool, output: &ToolOutput) -> Value {
    let mut payload = json!({"id": id, "tool": tool, "ok": ok, "output": output.to_text()});
    if !matches!(output.content(), [Content::Text(_)])
        && let Some(fields) = payload.as_object_mut()
    {
        let blocks = output.content().iter().map(|block| match block {
            Content::Image { mime_type, data } => {
                json!({"type": "image", "mime_type": mime_type, "data": BASE64.encode(data)})
            }
            text => json!({"type": "text", "text": text.to_string()}),
        });
        fields.insert("content".to_owned(), blocks.collect());
    }
    payload
}

/// What the `tool/result` event `id`, with `payload`, says the tool returned: its `content`
/// blocks if it has them, or else its `output` as one block of text.
fn recorded_output(id: EventId, payload: &Value) -> Result<ToolOutput> {
    let text = |value: &Value, name: &str| {
        let text = value.get(name).and_then(Value::as_str);
        text.unwrap_or_default().to_owned()
    };
    let Some(blocks) = payload.get("content").and_then(Value::as_array) else {
        return Ok(ToolOutput::text(text(payload, "output")));
    };
    blocks
        .iter()
        .map(|block| match block.get("type").and_then(Value::as_str) {
            Some("image") => {
                let data = BASE64
                    .decode(text(block, "data"))
                    .with_context(|| format!("event {id} has an image that is not base64"))?;
                Ok(Content::image(text(block, "mime_type"), data))
            }
            // A kind of block this version does not know stands as a note that it was there.
            Some(kind) if kind != "text" => Ok(Content::text(format!("[{kind} content]"))),
            _ => Ok(Content::text(text(block, "text"))),
        })
        .collect()
}

#[async_trait]
impl SessionLoader<Session> for Journal {
    async fn load(&self, _session_id: &str) -> Result<Session> {
        Ok(self.session())
    }
}

#[async_trait]
impl EffectHandler<Session, Effect> for Journal {
    async fn apply_effects(
        &self,
        _session: &Session,
        effects: &mut [Effect],
        _emit: &Emitter,
    ) -> Result<()> {
        let mut batch = Batch::default();
        for effect in effects {
            match effect {
                Effect::Message(message) => {
                    self.draft_message(&mut batch, message)?;
                }
                Effect::Usage(usage) => {
                    let payload = serde_json::to_string(usage).context("usage is not JSON")?;
                    batch
                        .drafts
                        .push(Draft::new(Source::Agent, USAGE).payload(payload));
                    let tokens = usage.usage.input_tokens.unwrap_or(0);
                    batch.input_tokens = Some(u64::try_from(tokens).unwrap_or(0));
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
                        .push((batch.drafts.len(), summary.clone(), *keep_from));
                    batch.drafts.push(
                        Draft::new(Source::Host, COMPACTION)
                            .payload(payload.to_string())
                            .cause(*keep_from),
                    );
                }
                Effect::Gated { call, decision } => {
                    let (draft, tool) = self.about_call(call, Source::Host, GATE_DECISION);
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
                        verdict: decision.verdict.clone(),
                        gate: decision.gate.clone(),
                        answer: None,
                    };
                    batch.gated.push((call.clone(), gated));
                }
            }
        }
        self.commit(batch)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

    /// `output` written as a `tool/result` and read back.
    fn round_trip(output: &ToolOutput) -> ToolOutput {
        let payload = result_payload("c1", "snap", true, output);
        recorded_output(EventId::new(1), &payload).unwrap()
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
    fn blocks_go_to_the_model_and_come_back_from_the_conversation_unchanged() {
        let output = ToolOutput::new(vec![
            Content::text("the view"),
            Content::image("image/png", PNG),
        ]);
        let blocks = blocks_of(&output);
        assert_eq!(
            blocks,
            [
                ContentBlock::text("the view"),
                ContentBlock::image("iVBORw0KGgo=", "image/png"),
            ]
        );
        assert_eq!(output_of(&CallToolResult::success(blocks)), output);
    }
}
