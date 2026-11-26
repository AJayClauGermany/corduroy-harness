//! What a run did and how long each part took, read off the log as text to hand to whoever
//! is looking into a problem.
//!
//! Nothing is measured here. Every commit already carries the time it was stored, so how
//! long the model took is the time from its `model/request` to what answered it, a tool from
//! its `tool/start` to its `tool/result`, and a person from the `gate/decision` that asked
//! to their `gate/answer`.

use std::collections::BTreeMap;

use corduroy_harness::MODEL_REQUEST;
use corduroy_harness::core::{Event, SessionId};
use serde_json::Value;

/// The most of one payload a report shows: the log has the rest.
const SHOWN: usize = 1000;

/// `time`, in milliseconds since the Unix epoch, as `2026-11-22T17:31:05.804Z`.
#[must_use]
pub fn stamp(time: u64) -> String {
    let seconds = time / 1000;
    let (days, of_day) = (seconds / 86_400, seconds % 86_400);
    // Days since 1970-01-01 to a date in the proleptic Gregorian calendar, by counting
    // from 0000-03-01 so that a leap day is the last day of its year.
    let shifted = days + 719_468;
    let (era, day_of_era) = (shifted / 146_097, shifted % 146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        of_day / 3600,
        of_day % 3600 / 60,
        of_day % 60,
        time % 1000
    )
}

/// The time from `from` to `to`, both in milliseconds, as `1.204s`.
fn span(from: u64, to: u64) -> String {
    let taken = to.saturating_sub(from);
    format!("{}.{:03}s", taken / 1000, taken % 1000)
}

/// Whether `event` is of `session` or of a session started from it.
#[must_use]
pub fn below(event: &Event, session: &SessionId) -> bool {
    event.session.as_ref().is_some_and(|id| {
        id == session
            || id
                .as_str()
                .strip_prefix(session.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// The runs of `session` in `events`, oldest first: each from the first event of the session
/// after the run before it ended, to its `turn/end`, or to the end of the log if it has none.
fn runs<'a>(events: &'a [Event], session: &SessionId) -> Vec<&'a [Event]> {
    let mut found = Vec::new();
    let mut rest = events;
    loop {
        let own = |event: &Event| event.session.as_ref() == Some(session);
        let Some(first) = rest
            .iter()
            .position(|event| own(event) && event.topic != "session/start")
        else {
            return found;
        };
        let (_, from) = rest.split_at(first);
        let end = from
            .iter()
            .position(|event| own(event) && event.topic == "turn/end")
            .map_or(from.len(), |last| last + 1);
        let (run, after) = from.split_at(end);
        found.push(run);
        rest = after;
    }
}

/// The payload of `event` as JSON, or null if it is not JSON.
fn payload(event: &Event) -> Value {
    serde_json::from_str(&event.payload).unwrap_or(Value::Null)
}

fn text<'a>(payload: &'a Value, name: &str) -> &'a str {
    payload
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// The counts a `usage` event may give, each with what it is called in a report.
const COUNTS: [(&str, &str); 5] = [
    ("input_tokens", "in"),
    ("output_tokens", "out"),
    ("cache_read_tokens", "cache read"),
    ("cache_write_tokens", "cache write"),
    ("reasoning_tokens", "reasoning"),
];

/// What the model requests of `run`, a run of `session`, used in all: how many answered,
/// and each count added up over those that gave it. The sessions the run started are in
/// it. Nothing if no request answered.
fn used(run: &[Event], session: &SessionId) -> Option<String> {
    let answers: Vec<Value> = run
        .iter()
        .filter(|event| event.topic == "usage" && below(event, session))
        .map(payload)
        .collect();
    if answers.is_empty() {
        return None;
    }
    let sums: Vec<String> = COUNTS
        .iter()
        .filter_map(|(key, name)| {
            let given: Vec<u64> = answers
                .iter()
                .filter_map(|answer| answer.get(*key)?.as_u64())
                .collect();
            let sum = given
                .iter()
                .fold(0_u64, |sum, count| sum.saturating_add(*count));
            (!given.is_empty()).then(|| format!("{sum} {name}"))
        })
        .collect();
    let requests = if answers.len() == 1 {
        "request"
    } else {
        "requests"
    };
    Some(format!(
        "used {} model {requests}: {} tokens",
        answers.len(),
        if sums.is_empty() {
            "no count of".to_owned()
        } else {
            sums.join(", ")
        }
    ))
}

/// The counts a `usage` event gave, as `10 in, 5 out, stop tool_calls`.
fn usage(payload: &Value) -> String {
    let mut parts: Vec<String> = COUNTS
        .iter()
        .filter_map(|(key, name)| {
            let count = payload.get(*key)?.as_u64()?;
            Some(format!("{count} {name}"))
        })
        .collect();
    let stop = text(payload, "stop");
    if !stop.is_empty() {
        parts.push(format!("stop {stop}"));
    }
    parts.join(", ")
}

/// Something a run was waiting on: a model, a tool or a person.
struct Open {
    /// Which of the lines is about it.
    at: usize,
    /// When the wait began.
    since: u64,
    /// The line about it, up to how it ended.
    line: String,
}

/// What an event says about a wait, which is named by its kind, its session and, for a tool
/// call, the call's id: one session asks its model one thing at a time.
enum Change {
    /// The wait of that name began, described so.
    Begun(String, String),
    /// The wait of that name ended, in this way.
    Ended(String, String),
    /// Nothing.
    None,
}

/// What `event`, of `session`, says about a wait.
fn change(event: &Event, session: &str) -> Change {
    let payload = payload(event);
    let call = text(&payload, "id");
    let tool = text(&payload, "tool");
    let named = |kind: &str| format!("{kind} {session} {call}");
    let model = format!("model {session}");
    let count = |name: &str| payload.get(name).and_then(Value::as_u64).unwrap_or(0);
    match event.topic.as_str() {
        MODEL_REQUEST => Change::Begun(
            model,
            format!(
                "model   {}/{} asked with {} messages and {} tools in `{session}`",
                text(&payload, "provider"),
                text(&payload, "model"),
                count("messages"),
                count("tools"),
            ),
        ),
        "usage" => Change::Ended(model, format!("answered: {}", usage(&payload))),
        "turn/end" => {
            let (status, reason) = (text(&payload, "status"), text(&payload, "reason"));
            let how = format!("no answer: the run ended as {status} {reason}");
            Change::Ended(model, how.trim_end().to_owned())
        }
        "tool/start" => Change::Begun(
            named("tool"),
            format!("tool    {tool} ({call}) in `{session}`"),
        ),
        "tool/result" => {
            let ok = payload.get("ok") == Some(&Value::Bool(true));
            let how = if ok { "returned" } else { "failed" };
            Change::Ended(named("tool"), how.to_owned())
        }
        "tool/wait" => {
            let how = format!("waits for the signal {}", text(&payload, "signal"));
            Change::Ended(named("tool"), how)
        }
        "gate/decision" if text(&payload, "verdict") == "ask" => Change::Begun(
            named("person"),
            format!(
                "person  {} asked about {tool} ({call}) in `{session}`",
                text(&payload, "gate")
            ),
        ),
        "gate/answer" => {
            let allowed = payload.get("allow") == Some(&Value::Bool(true));
            let how = if allowed { "allowed" } else { "refused" };
            Change::Ended(named("person"), how.to_owned())
        }
        _ => Change::None,
    }
}

/// What `run` waited on, one line each in the order the waits began: every model request,
/// tool call and question to a person, with how long it took and how it ended.
fn waits(run: &[Event]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut open: BTreeMap<String, Open> = BTreeMap::new();
    for event in run {
        let Some(session) = event.session.as_ref().map(SessionId::as_str) else {
            continue;
        };
        match change(event, session) {
            Change::Begun(name, line) => {
                let (at, since) = (lines.len(), event.time);
                lines.push(String::new());
                open.insert(name, Open { at, since, line });
            }
            Change::Ended(name, how) => {
                if let Some(waited) = open.remove(&name)
                    && let Some(line) = lines.get_mut(waited.at)
                {
                    let took = span(waited.since, event.time);
                    *line = format!("{took:>9}  {}: {how}", waited.line);
                }
            }
            Change::None => {}
        }
    }
    for waited in open.into_values() {
        if let Some(line) = lines.get_mut(waited.at) {
            *line = format!("{:>9}  {}: nothing in the log answers it", "?", waited.line);
        }
    }
    lines
}

/// One event as a line of a run that began at `start`, after an event stored at `before`:
/// when, how long after the run began and after the event before it, and the event, with at
/// most [`SHOWN`] characters of its payload.
fn line(event: &Event, start: u64, before: u64) -> String {
    let mut parts = vec![format!(
        "{} +{:<9} {:>8}  {}",
        stamp(event.time),
        span(start, event.time),
        span(before, event.time),
        event.id
    )];
    if let Some(session) = &event.session {
        parts.push(session.to_string());
    }
    parts.push(format!("[{}] {}", event.source, event.topic));
    if let Some(cause) = event.cause {
        parts.push(format!("(after {cause})"));
    }
    let length = event.payload.chars().count();
    if length > SHOWN {
        let shown: String = event.payload.chars().take(SHOWN).collect();
        let cut = length - SHOWN;
        parts.push(format!("{shown}… (+{cut} characters, whole in the log)"));
    } else if length > 0 {
        parts.push(event.payload.clone());
    }
    parts.join(" ")
}

/// The last `count` runs of `session` in `events` (the whole log, in order), as text: for
/// each run what wrote it, what it waited on and for how long, then every event stored
/// while it went, each with its time.
#[must_use]
pub fn report(events: &[Event], session: &SessionId, count: usize) -> String {
    let all = runs(events, session);
    if all.is_empty() {
        return format!("`{session}` has no run in the log");
    }
    let total = all.len();
    let skipped = total.saturating_sub(count.max(1));
    let mut out: Vec<String> = Vec::new();
    for (number, run) in all.into_iter().enumerate().skip(skipped) {
        let (Some(first), Some(last)) = (run.first(), run.last()) else {
            continue;
        };
        let ended = run
            .iter()
            .rev()
            .find(|event| event.session.as_ref() == Some(session) && event.topic == "turn/end")
            .map_or_else(
                || "unfinished".to_owned(),
                |end| {
                    let end = payload(end);
                    format!("{} {}", text(&end, "status"), text(&end, "reason"))
                },
            );
        out.push(format!(
            "\nrun {} of {total} in `{session}`: {} to {}, {}, {}",
            number + 1,
            first.id,
            last.id,
            span(first.time, last.time),
            ended.trim_end()
        ));
        // What the process that wrote the run's first event ran as.
        let writer = events
            .iter()
            .take_while(|event| event.id <= first.id)
            .filter(|event| event.topic == "process/start")
            .last();
        if let Some(writer) = writer {
            out.push(format!("written by {}", writer.payload));
        }
        out.extend(used(run, session));
        out.push("\nwaited on".to_owned());
        out.extend(waits(run));
        out.push("\nevents (stored at, since the run began, since the event before)".to_owned());
        let mut before = first.time;
        for event in run {
            if event.session.is_none() || below(event, session) {
                out.push(line(event, first.time, before));
                before = event.time;
            }
        }
    }
    out.join("\n").trim().to_owned()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use corduroy_harness::core::{Draft, EventStream, Source};

    use super::*;

    #[test]
    fn times_are_shown_as_utc_dates_to_the_millisecond() {
        assert_eq!(stamp(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(stamp(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(stamp(1_795_368_645_804), "2026-11-22T17:30:45.804Z");
        assert_eq!(stamp(4_102_444_799_999), "2099-12-31T23:59:59.999Z");
    }

    /// A stream whose commits are stored 250 ms apart, from one second after the epoch.
    fn ticking() -> EventStream {
        let ticks = Arc::new(AtomicU32::new(0));
        EventStream::new()
            .with_clock(move || 1000 + 250 * u64::from(ticks.fetch_add(1, Ordering::SeqCst)))
    }

    fn put(stream: &EventStream, session: &str, source: Source, topic: &str, payload: &str) {
        let draft = Draft::new(source, topic)
            .session(SessionId::new(session))
            .payload(payload);
        stream.emit(draft).unwrap();
    }

    #[test]
    fn a_run_says_what_it_waited_on_and_for_how_long() {
        let stream = ticking();
        let host = || Source::Host;
        put(&stream, "main", Source::User, "message", "{}");
        let asked = r#"{"provider":"p","model":"m","messages":1,"tools":2}"#;
        put(&stream, "main", host(), "model/request", asked);
        let used = r#"{"model":"m","input_tokens":10,"output_tokens":5,"stop":"tool_calls"}"#;
        put(&stream, "main", Source::Agent, "usage", used);
        put(
            &stream,
            "main",
            host(),
            "tool/start",
            r#"{"id":"c1","tool":"bash"}"#,
        );
        put(&stream, "other", Source::User, "message", "{}");
        let result = r#"{"id":"c1","tool":"bash","ok":false,"output":"no"}"#;
        put(&stream, "main", Source::Environment, "tool/result", result);
        put(&stream, "main", host(), "model/request", asked);
        let failed = r#"{"status":"failed","reason":"401 Unauthorized"}"#;
        put(&stream, "main", host(), "turn/end", failed);

        let said = report(&stream.events(), &SessionId::new("main"), 1);
        assert!(
            said.contains("run 1 of 1 in `main`: #1 to #8, 1.750s, failed 401 Unauthorized"),
            "{said}"
        );
        // What the run used in all: the one request that answered.
        assert!(
            said.contains("\nused 1 model request: 10 in, 5 out tokens\n"),
            "{said}"
        );
        let answered = "0.250s  model   p/m asked with 1 messages and 2 tools in `main`: \
                        answered: 10 in, 5 out, stop tool_calls";
        assert!(said.contains(answered), "{said}");
        assert!(
            said.contains("0.500s  tool    bash (c1) in `main`: failed"),
            "{said}"
        );
        assert!(
            said.contains("no answer: the run ended as failed 401 Unauthorized"),
            "{said}"
        );
        // Each event has its time, and another session's events are left out.
        assert!(
            said.contains(
                "1970-01-01T00:00:01.250Z +0.250s      0.250s  #2 main [host] model/request"
            ),
            "{said}"
        );
        assert!(!said.contains("other"), "{said}");
    }

    #[test]
    fn only_the_runs_asked_for_are_shown_and_a_run_without_an_end_is_unfinished() {
        let stream = ticking();
        put(&stream, "main", Source::User, "message", "{}");
        put(
            &stream,
            "main",
            Source::Host,
            "turn/end",
            r#"{"status":"finished"}"#,
        );
        put(&stream, "main", Source::User, "message", "{}");
        let asked = r#"{"provider":"p","model":"m","messages":1,"tools":0}"#;
        put(&stream, "main", Source::Host, "model/request", asked);
        put(
            &stream,
            "main/1",
            Source::User,
            "message",
            &"x".repeat(SHOWN + 7),
        );
        let events = stream.events();
        let main = SessionId::new("main");

        let last = report(&events, &main, 1);
        assert!(last.contains("run 2 of 2 in `main`: #3 to #5, 0.500s, unfinished"));
        assert!(!last.contains("run 1 of 2"), "{last}");
        assert!(last.contains("nothing in the log answers it"), "{last}");
        // A session started from this one is part of the run, and a long payload is cut.
        assert!(last.contains("#5 main/1 [user] message"), "{last}");
        assert!(
            last.contains("… (+7 characters, whole in the log)"),
            "{last}"
        );

        assert!(report(&events, &main, 2).contains("run 1 of 2 in `main`: #1 to #2"));
        assert_eq!(
            report(&events, &SessionId::new("none"), 1),
            "`none` has no run in the log"
        );
    }
}
