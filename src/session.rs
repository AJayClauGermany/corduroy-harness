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
//! | `gate/decision` | `host` | `{"point", "id", "tool", "verdict", "reason", "gate"}`, caused by its `tool/call`: what the gates said |
//! | `gate/answer` | `user` | `{"id", "tool", "allow", "reason"}`, caused by its `tool/call`: a person's answer to a gate that asked |
//! | `tool/start` | `host` | `{"id", "tool"}`, caused by its `tool/call`: the tool is about to run |
//! | `tool/result` | `environment` | `{"id", "tool", "ok", "output"}`, caused by its `tool/call` |
//! | `usage` | `agent` | the provider's usage report, as JSON |
//! | `turn/end` | `host` | empty |
//! | `run/restart` | `host` | `{"attempt"}`: an unfinished run was picked up again |
//!
//! A tool call leaves a trail of its own, one commit at a time: `tool/call` with the model's
//! message, `tool/start` just before the tool runs, `tool/result` as soon as it returns, and
//! only then the `message` that carries the results of every call of that step to the
//! model. After a crash the log therefore says of each call whether it never started, was
//! running, or finished with a result that only needs handing over.
//!
//! Where a gate guards tool calls, its decision on each call is stored before the tool can
//! start. A call a gate asked about waits for a `gate/answer`.
//!
//! A run is *unfinished* from its first `message` until its `turn/end`. A session whose
//! events stop short of that was interrupted, and [`Harness::continue_run`] picks it up,
//! unless it is *waiting*: a gate asked about one of its calls and nobody has answered yet.
//! [`Harness::answer`] gives the answer and takes the run on.
//!
//! [`Harness::continue_run`]: crate::Harness::continue_run
//! [`Harness::answer`]: crate::Harness::answer

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use corduroy_core::{
    Decision, Draft, Event, EventId, EventStream, LogError, Pattern, SessionId, Source, TOOL_CALL,
    Verdict,
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
use rmcp::model::Role;
use serde_json::{Value, json};

/// Topic of the events that make up the conversation.
pub const MESSAGE: &str = "message";

/// Topic of the event that ends a run.
const TURN_END: &str = "turn/end";

/// Topic of the event that records an unfinished run being picked up again.
const RESTART: &str = "run/restart";

/// Topic of the event that records what the gates decided about a tool call.
const GATE_DECISION: &str = "gate/decision";

/// Topic of the event that records a person's answer to a gate that asked.
const GATE_ANSWER: &str = "gate/answer";

/// Topic of the event that begins a session.
pub const SESSION_START: &str = "session/start";

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
}

impl SessionInfo {
    /// The event that begins this session.
    pub(crate) fn start(&self) -> Draft {
        let payload = self
            .parent
            .as_ref()
            .map_or_else(|| json!({}), |parent| json!({"parent": parent.as_str()}));
        Draft::new(Source::Host, SESSION_START)
            .session(self.id.clone())
            .payload(payload.to_string())
    }

    /// Every session `events` holds, in the order they were started.
    pub(crate) fn all(events: &EventStream) -> Vec<Self> {
        events
            .matching(&Pattern::new(SESSION_START))
            .into_iter()
            .filter_map(|event| {
                let payload: Value = serde_json::from_str(&event.payload).ok()?;
                let parent = payload.get("parent").and_then(Value::as_str);
                Some(Self {
                    id: event.session?,
                    parent: parent.map(SessionId::new),
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
        let info = SessionInfo {
            id,
            parent: parent.cloned(),
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
    result: Option<(bool, String)>,
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
        /// What the tool returned.
        output: String,
    },
}

struct State {
    conversation: Conversation,
    /// The tool calls waiting for a response, by tool-call id.
    calls: BTreeMap<String, OpenCall>,
    /// Whether a run has begun and not reached its `turn/end`.
    unfinished: bool,
    /// How many times the unfinished run has been picked up again.
    restarts: u32,
}

/// One step's worth of events, and what they change in the projection once stored.
#[derive(Default)]
struct Batch {
    drafts: Vec<Draft>,
    messages: Vec<Message>,
    /// Tool-call id to the position of its `tool/call` draft, the tool's name and its input.
    opened: BTreeMap<String, (usize, String, String)>,
    /// Ids of the tool calls answered in this batch.
    closed: Vec<String>,
    /// What the gates decided about tool calls, by tool-call id.
    gated: Vec<(String, Gated)>,
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
            Ok(result) => (result.is_error != Some(true), text_of(result)),
            Err(error) => (false, error.message.to_string()),
        };
        let payload = json!({"id": response.id, "tool": tool, "ok": ok, "output": output});
        self.drafts.push(draft.payload(payload.to_string()));
    }
}

impl State {
    /// Bring the projection up to date with one stored event of the session.
    fn project(&mut self, event: &Event) -> Result<()> {
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
                self.conversation.push(message);
                self.unfinished = true;
            }
            TURN_END => (self.unfinished, self.restarts) = (false, 0),
            RESTART => self.restarts = self.restarts.saturating_add(1),
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
                    call.result = Some((ok, text("output").unwrap_or_default()));
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
        batch.messages.push(message.clone());
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
    pub(crate) fn finish_call(&self, call: &str, ok: bool, output: &str) -> Result<()> {
        let (draft, tool) = self.about_call(call, Source::Environment, "tool/result");
        let payload = json!({"id": call, "tool": tool, "ok": ok, "output": output});
        self.events.emit(draft.payload(payload.to_string()))?;
        if let Some(open) = self.state().calls.get_mut(call) {
            open.result = Some((ok, output.to_owned()));
        }
        Ok(())
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
        for message in batch.messages {
            state.conversation.push(message);
            state.unfinished = true;
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
        drop(state);
        Ok(ids)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The text blocks of a tool result, joined.
fn text_of(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
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
                        .push(Draft::new(Source::Agent, "usage").payload(payload));
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
