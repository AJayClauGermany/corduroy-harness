//! What the page is served: its own files, the events as the scripts it loads, and the one
//! thing it can ask for, an answer recorded.
//!
//! Every request has to come to this machine's own address ([`Served::is_ours`]: a name
//! that merely resolves here is someone else's page) and carry the token in its path
//! ([`Served::within`]), or it is refused without saying which was wrong.

use std::sync::{Arc, Mutex, PoisonError};

use corduroy_harness::core::{Event, EventId, EventStream, SessionId};
use corduroy_harness::{Answer, HarnessError, Outside};
use serde_json::{Value, json};

use super::http::{POLICY, Request, Response};

/// The page, as the `viewer` add-on also writes it: one page for both.
const HTML: &str = include_str!("../../../../plugins/viewer/page/index.html");
const APP: &str = include_str!("../../../../plugins/viewer/page/app.js");
const STYLE: &str = include_str!("../../../../plugins/viewer/page/style.css");

/// What the page's own file allows itself, opened as a file. Served, it is allowed what
/// [`POLICY`] says, which lets it send an answer back.
const FILE_POLICY: &str = "default-src 'none'; script-src 'self' file:; style-src 'self' file:; \
                           base-uri 'none'; form-action 'none'";

/// What the page is served under its address, besides the chunks of events.
const FILES: [&str; 8] = [
    "",
    "index.html",
    "app.js",
    "style.css",
    "index.js",
    "answer",
    "say",
    "stop",
];
/// The format of the scripts, which the page checks: the `viewer` add-on's.
const FORMAT: u32 = 1;
/// Events to a chunk, by id: chunk 0 holds ids 1 to 200.
const CHUNK: u64 = 200;
/// A payload longer than this is cut, and says so: the log has it whole.
const MAX_PAYLOAD: usize = 256 * 1024;
/// Characters kept of a payload that is cut.
const KEPT: usize = 4000;
/// What a refusal with no reason given is recorded as, as at the terminal.
const DECLINED: &str = "the user declined";

/// The page as it is served: allowed what the response's own header allows it. A browser
/// takes `frame-ancestors` from a header only, so the page's copy of the policy leaves it
/// out.
fn page() -> String {
    HTML.replace(FILE_POLICY, &POLICY.replace("; frame-ancestors 'none'", ""))
}

/// What one page is served from.
pub(super) struct Served {
    /// The plugin serving, which an answer is recorded as given through.
    pub(super) name: String,
    /// The port listened on.
    pub(super) port: u16,
    /// What a request's path has to begin with.
    pub(super) token: String,
    /// The record.
    pub(super) events: Arc<EventStream>,
    /// Where an answer is recorded.
    pub(super) outside: Arc<Outside>,
    /// The id of the last event seen, so that finding the next is not a search.
    pub(super) last: Mutex<u64>,
}

/// Where a request's path leads.
enum Place<'a> {
    /// Under the token: what follows it.
    Within(&'a str),
    /// The token itself, without the `/` its files are found under.
    Door,
    /// Somewhere else.
    Outside,
}

/// Whether `given` is `token`, taking the same time whatever `given` is.
fn same(given: &str, token: &str) -> bool {
    given.len() == token.len()
        && given
            .bytes()
            .zip(token.bytes())
            .fold(0, |differ, (a, b)| differ | (a ^ b))
            == 0
}

/// The answer to a request that did not say what it has to.
fn wanted(what: &str) -> Response {
    Response::json(400, &json!({"error": what}))
}

/// The answer to something the page asked to have recorded: `recorded`, or why not, in
/// words the page shows.
fn done(recorded: Result<Value, HarnessError>) -> Response {
    let error = match recorded {
        Ok(recorded) => return Response::json(200, &recorded),
        Err(error) => error,
    };
    let (status, said) = match &error {
        HarnessError::NotAsked { .. } => {
            (409, format!("{error}: it may have been answered already"))
        }
        HarnessError::UnknownSession(_) => (404, error.to_string()),
        // A run is under way, or the host does not take what was asked.
        HarnessError::Waiting(_) | HarnessError::Unfinished(_) | HarnessError::Run(_) => {
            (409, error.to_string())
        }
        _ => (500, error.to_string()),
    };
    Response::json(status, &json!({"error": said}))
}

/// `event` as the page reads it: its fields, with its payload as the JSON it is, or as
/// text where it is not JSON or was cut.
fn shown(event: &Event) -> Value {
    let mut cut = None;
    let payload = if event.payload.len() > MAX_PAYLOAD {
        cut = Some(event.payload.len());
        Value::String(event.payload.chars().take(KEPT).collect())
    } else {
        serde_json::from_str(&event.payload)
            .unwrap_or_else(|_| Value::String(event.payload.clone()))
    };
    let mut shown = json!({
        "id": event.id.get(),
        "time": event.time,
        "session": event.session.as_ref().map(SessionId::as_str),
        "source": event.source.to_string(),
        "cause": event.cause.map(EventId::get),
        "topic": event.topic,
        "payload": payload,
    });
    if let (Some(cut), Some(fields)) = (cut, shown.as_object_mut()) {
        fields.insert("cut".to_owned(), json!(cut));
    }
    shown
}

impl Served {
    /// Whether a request sent to `host` was sent to this machine by its own address, and
    /// not to a name someone pointed here.
    fn is_ours(&self, host: Option<&str>) -> bool {
        host.is_some_and(|host| {
            let host = host.to_ascii_lowercase();
            host == format!("127.0.0.1:{}", self.port) || host == format!("localhost:{}", self.port)
        })
    }

    /// Whether `origin`, the page a request says it comes from, is this one.
    fn is_us(&self, origin: Option<&str>) -> bool {
        origin.is_some_and(|origin| {
            let origin = origin.to_ascii_lowercase();
            origin == format!("http://127.0.0.1:{}", self.port)
                || origin == format!("http://localhost:{}", self.port)
        })
    }

    /// Where `path` leads.
    fn within<'a>(&self, path: &'a str) -> Place<'a> {
        let Some(path) = path.strip_prefix('/') else {
            return Place::Outside;
        };
        match path.split_once('/') {
            Some((token, rest)) if same(token, &self.token) => Place::Within(rest),
            None if same(path, &self.token) => Place::Door,
            _ => Place::Outside,
        }
    }

    /// The id of the last event in the record.
    fn last(&self) -> u64 {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let newer = self.events.after(EventId::new(*last), usize::MAX, |_| true);
        if let Some(event) = newer.last() {
            *last = event.id.get();
        }
        *last
    }

    /// The index: how far the events go, and in how many chunks.
    fn index(&self) -> Response {
        let last = self.last();
        let index = json!({
            "format": FORMAT,
            "generation": 1,
            "chunks": last.div_ceil(CHUNK),
            "last": last,
            "events": self.events.len(),
            "behind": false,
            // Whether a message typed on the page would be run.
            "heard": self.outside.is_heard(),
        });
        Response::new(200, "text/javascript", format!("corduroyIndex({index});\n"))
    }

    /// Chunk `number`: the events whose ids fall in it.
    fn chunk(&self, number: u64) -> Response {
        let from = number.saturating_mul(CHUNK);
        let to = from.saturating_add(CHUNK);
        let limit = usize::try_from(CHUNK).unwrap_or(usize::MAX);
        let events: Vec<Value> = self
            .events
            .after(EventId::new(from), limit, |_| true)
            .iter()
            .filter(|event| event.id.get() <= to)
            .map(shown)
            .collect();
        let script = format!("corduroyChunk({number},{});\n", Value::Array(events));
        Response::new(200, "text/javascript", script)
    }

    /// What `request`, posted by the page, holds: JSON, from this page and no other.
    fn posted(&self, request: &Request) -> Result<Value, Response> {
        // A page other than this one cannot say it is this one: the browser says.
        if !self.is_us(request.header("origin")) {
            return Err(Response::plain(403, "this is taken from the page only"));
        }
        let json = request
            .header("content-type")
            .is_some_and(|kind| kind.to_ascii_lowercase().starts_with("application/json"));
        json.then(|| serde_json::from_slice(&request.body).ok())
            .flatten()
            .ok_or_else(|| Response::json(400, &json!({"error": "the page posts JSON"})))
    }

    /// Record the answer `asked` holds, as the person's, given through this page.
    fn answer(&self, asked: &Value) -> Response {
        let read = (|| {
            Some((
                asked.get("session")?.as_str()?,
                asked.get("call")?.as_str()?,
                asked.get("allow")?.as_bool()?,
                asked.get("reason").and_then(Value::as_str).unwrap_or(""),
            ))
        })();
        let Some((session, call, allow, reason)) = read else {
            return wanted("an answer is {\"session\", \"call\", \"allow\", \"reason\"}");
        };
        let answer = match (allow, reason.trim()) {
            (true, _) => Answer::Allow,
            (false, "") => Answer::Deny(DECLINED.to_owned()),
            (false, reason) => Answer::Deny(reason.to_owned()),
        };
        let recorded = self
            .outside
            .answer(&self.name, &SessionId::new(session), call, &answer);
        done(recorded.map(|()| json!({"ok": true})))
    }

    /// Record what `asked` holds as said to a session by the person, through this page.
    fn say(&self, asked: &Value) -> Response {
        let read = (|| {
            Some((
                asked.get("session")?.as_str()?,
                asked.get("text")?.as_str()?,
            ))
        })();
        let Some((session, text)) = read else {
            return wanted("a message is {\"session\", \"text\"}");
        };
        let said = self.outside.say(&self.name, &SessionId::new(session), text);
        done(said.map(|()| json!({"ok": true})))
    }

    /// Ask the run of the session `asked` names to stop, through this page.
    fn stop(&self, asked: &Value) -> Response {
        let Some(session) = asked.get("session").and_then(Value::as_str) else {
            return wanted("a stop is {\"session\"}");
        };
        match self.outside.stop(&self.name, &SessionId::new(session)) {
            Ok(false) => {
                let said = format!("`{session}` has no run to stop");
                Response::json(409, &json!({"error": said}))
            }
            stopped => done(stopped.map(|_| json!({"ok": true}))),
        }
    }

    /// What `request` is answered with.
    #[must_use]
    pub(super) fn answer_to(&self, request: &Request) -> Response {
        let refused = || {
            Response::plain(
                403,
                "This is not the page's address. corduroy says the address each time it \
                 starts, and it is a new one each time.",
            )
        };
        if !self.is_ours(request.header("host")) {
            return refused();
        }
        let rest = match self.within(request.path()) {
            Place::Within(rest) => rest,
            Place::Door => return Response::to(format!("/{}/", self.token)),
            Place::Outside => return refused(),
        };
        let chunk = rest
            .strip_prefix("events-")
            .and_then(|name| name.strip_suffix(".js"))
            .and_then(|number| number.parse::<u64>().ok());
        match (request.method.as_str(), rest, chunk) {
            ("GET", "" | "index.html", _) => Response::new(200, "text/html; charset=utf-8", page()),
            ("GET", "app.js", _) => Response::new(200, "text/javascript", APP),
            ("GET", "style.css", _) => Response::new(200, "text/css", STYLE),
            ("GET", "index.js", _) => self.index(),
            ("GET", _, Some(number)) => self.chunk(number),
            ("POST", "answer" | "say" | "stop", _) => match (self.posted(request), rest) {
                (Err(refused), _) => refused,
                (Ok(asked), "answer") => self.answer(&asked),
                (Ok(asked), "say") => self.say(&asked),
                (Ok(asked), _) => self.stop(&asked),
            },
            (_, file, chunk) if chunk.is_some() || FILES.contains(&file) => {
                Response::plain(405, "not that way")
            }
            _ => Response::plain(404, "the page has no such file"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_served_may_send_an_answer_and_the_page_as_a_file_may_not() {
        assert!(HTML.contains(FILE_POLICY), "the page's own policy changed");
        let served = page();
        assert!(served.contains("connect-src 'self'"));
        assert!(!served.contains("file:") && !served.contains("frame-ancestors"));
        assert!(POLICY.contains("; frame-ancestors 'none'"));
        assert!(!HTML.contains("connect-src"));
    }

    #[test]
    fn a_token_is_itself_and_nothing_near_it() {
        assert!(same("abc123", "abc123"));
        for given in ["", "abc12", "abc1234", "abc124", "ABC123"] {
            assert!(!same(given, "abc123"), "{given}");
        }
    }
}
