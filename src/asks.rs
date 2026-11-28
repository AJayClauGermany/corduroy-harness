//! Asks put to a person anywhere but a tool call of the model's, kept in the log.
//!
//! **A prototype.** That an ask at any declared point can reach a person, and that one
//! `gate/answer` names whichever it was, is decided. What the call that led to the ask does
//! meanwhile is not: this is the form in which it *fails and is made again*, tried first
//! because it is the smaller; the form in which it waits is still open (C15 in
//! `docs/open-questions.md`). `docs/prototypes.md` says what this found.
//!
//! A host that will show a person the asks and take their answers says so
//! ([`Harness::answers_asks`](crate::Harness::answers_asks)). From then on, when a gate
//! asks at `plugin/load` or `session/start`:
//!
//! 1. The ask is recorded as a `gate/decision` with the verdict `ask` and an `ask` id, and
//!    what was asked about is refused, with a reason that says a person was asked.
//! 2. The host lists the asks that wait ([`Harness::open_asks`](crate::Harness::open_asks))
//!    and records the person's answer
//!    ([`Harness::answer_ask`](crate::Harness::answer_ask)) as a `gate/answer` naming that
//!    id.
//! 3. The next time the same thing is asked for, the answer is found: allowed, it goes
//!    through; refused, it fails with the person's reason. Either way a `gate/decision`
//!    records that the answer was used, and the same request after that is a new ask.
//!
//! An ask's id is made from the point and the request, so "the same thing" is the same
//! request, to the letter. Only what the host recorded counts: a `gate/answer` a plugin
//! raised is not a person's.

use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use corduroy_core::sync::Lock;
use corduroy_core::{
    Asked, Asks, Decision, Draft, Event, EventId, EventStream, LogError, Query, SessionId, Source,
};
use serde_json::{Value, json};

use crate::session::Answer;

/// How many events one read of the log asks for.
const PAGE: usize = 1000;

/// An ask that waits for a person's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct OpenAsk {
    /// What names the ask, for [`Harness::answer_ask`](crate::Harness::answer_ask).
    pub id: String,
    /// The decision point it was asked at.
    pub point: String,
    /// The session it was asked for, if it was asked while one was served.
    pub session: Option<SessionId>,
    /// What the gates were asked, as JSON text.
    pub request: String,
    /// Why the gate asked.
    pub reason: String,
    /// The gate that asked.
    pub gate: String,
}

/// How an ask stands, as the log says.
enum Standing {
    /// A person has not answered.
    Waiting(OpenAsk, EventId),
    /// A person answered, and nothing has used the answer yet.
    Answered {
        allow: bool,
        reason: String,
        answer: EventId,
    },
}

/// The [`Asks`] that keeps asks and answers in the log.
pub(crate) struct LogAsks {
    events: Arc<EventStream>,
    /// Held while an ask is settled, so that one answer is used once.
    settling: Lock<()>,
}

/// A name for the ask about `request` at `point` that the same ask always gets: its 64-bit
/// FNV-1a hash, as sixteen hexadecimal digits.
fn id_of(point: &str, request: &str) -> String {
    let hashed = point
        .bytes()
        .chain(*b"\n")
        .chain(request.bytes())
        .fold(0xcbf2_9ce4_8422_2325_u64, |hashed, byte| {
            (hashed ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    format!("{hashed:016x}")
}

impl LogAsks {
    pub(crate) const fn new(events: Arc<EventStream>) -> Self {
        Self {
            events,
            settling: Lock::new(()),
        }
    }

    /// How every ask that is not over stands, by id.
    fn standing(&self) -> Result<BTreeMap<String, Standing>, LogError> {
        let mut standing = BTreeMap::new();
        let mut query = Query::first(PAGE).topics("gate/*");
        loop {
            let page = self.events.read_where(&query, |_| true)?;
            let full = page.len() >= PAGE;
            let Some(last) = page.last().map(|event| event.id) else {
                break;
            };
            for event in &page {
                note(&mut standing, event);
            }
            if !full {
                break;
            }
            query = query.after(last);
        }
        Ok(standing)
    }

    /// The asks that wait for a person, oldest first.
    pub(crate) fn open(&self) -> Result<Vec<OpenAsk>, LogError> {
        let mut open: Vec<(EventId, OpenAsk)> = self
            .standing()?
            .into_values()
            .filter_map(|standing| match standing {
                Standing::Waiting(ask, asked) => Some((asked, ask)),
                Standing::Answered { .. } => None,
            })
            .collect();
        open.sort_by_key(|(asked, _)| *asked);
        Ok(open.into_iter().map(|(_, ask)| ask).collect())
    }

    /// Record a person's `answer` to the ask `id`, given through the plugin `via` if not at
    /// the host itself. Returns whether an ask of that id was waiting.
    pub(crate) fn answer(
        &self,
        id: &str,
        answer: &Answer,
        via: Option<&str>,
    ) -> Result<bool, LogError> {
        self.settling.with(|()| {
            let Some(Standing::Waiting(ask, asked)) = self.standing()?.remove(id) else {
                return Ok(false);
            };
            let (allow, reason) = match answer {
                Answer::Allow => (true, ""),
                Answer::Deny(reason) => (false, reason.as_str()),
            };
            let mut payload =
                json!({"point": ask.point, "ask": id, "allow": allow, "reason": reason});
            if let (Some(via), Some(fields)) = (via, payload.as_object_mut()) {
                fields.insert("via".to_owned(), json!(via));
            }
            let draft = Draft::new(Source::User, "gate/answer")
                .cause(asked)
                .payload(payload.to_string());
            self.events.emit(in_session(draft, ask.session))?;
            Ok(true)
        })
    }
}

/// `draft` as an event of `session`, if there is one.
fn in_session(draft: Draft, session: Option<SessionId>) -> Draft {
    match session {
        Some(session) => draft.session(session),
        None => draft,
    }
}

/// Bring `standing` up to date with `event`, if it is about an ask: the host's record of
/// one, a person's answer to one that waits, or the host's record that an answer was used.
fn note(standing: &mut BTreeMap<String, Standing>, event: &Event) {
    let Ok(payload) = serde_json::from_str::<Value>(&event.payload) else {
        return;
    };
    let text = |name: &str| {
        payload
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
    };
    let id = text("ask");
    if id.is_empty() {
        return;
    }
    match (event.topic.as_str(), &event.source) {
        ("gate/decision", Source::Host) if text("verdict") == "ask" => {
            let ask = OpenAsk {
                id: id.to_owned(),
                point: text("point").to_owned(),
                session: event.session.clone(),
                request: text("request").to_owned(),
                reason: text("reason").to_owned(),
                gate: text("gate").to_owned(),
            };
            standing.insert(id.to_owned(), Standing::Waiting(ask, event.id));
        }
        // The answer was used: the ask is over.
        ("gate/decision", Source::Host) => drop(standing.remove(id)),
        ("gate/answer", Source::User) => {
            if let Some(Standing::Waiting(..)) = standing.get(id) {
                let answered = Standing::Answered {
                    allow: payload.get("allow").and_then(Value::as_bool) == Some(true),
                    reason: text("reason").to_owned(),
                    answer: event.id,
                };
                standing.insert(id.to_owned(), answered);
            }
        }
        _ => {}
    }
}

impl Asks for LogAsks {
    fn settle(&self, point: &str, request: &str, decision: &Decision) -> Asked {
        let id = id_of(point, request);
        let session = corduroy_core::call::current().map(|scope| scope.session);
        let record = |verdict: &str, reason: &str, answer: Option<EventId>| {
            let mut payload = json!({
                "point": point, "ask": id, "request": request, "verdict": verdict,
                "reason": reason, "gate": decision.gate,
            });
            let mut draft = Draft::new(Source::Host, "gate/decision");
            if let (Some(answer), Some(fields)) = (answer, payload.as_object_mut()) {
                fields.insert("answer".to_owned(), json!(answer.get()));
                draft = draft.cause(answer);
            }
            self.events.emit(in_session(
                draft.payload(payload.to_string()),
                session.clone(),
            ))
        };
        let settled = self.settling.with(|()| {
            Ok::<_, LogError>(match self.standing()?.remove(&id) {
                None => {
                    record("ask", decision.verdict.reason(), None)?;
                    Asked::Waiting(id.clone())
                }
                Some(Standing::Waiting(..)) => Asked::Waiting(id.clone()),
                Some(Standing::Answered {
                    allow: true,
                    answer,
                    ..
                }) => {
                    record("allow", "", Some(answer))?;
                    Asked::Allowed
                }
                Some(Standing::Answered { reason, answer, .. }) => {
                    record("deny", &reason, Some(answer))?;
                    Asked::Refused(reason)
                }
            })
        });
        // An ask that cannot be recorded or read reaches nobody: it is refused.
        settled.unwrap_or_else(|error| Asked::Refused(error.to_string()))
    }
}
