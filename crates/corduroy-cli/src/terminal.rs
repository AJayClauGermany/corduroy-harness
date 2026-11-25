//! The agent at a terminal: what it does, shown as it happens, and a person answering the
//! gates that ask.
//!
//! Nothing here keeps state of its own. What is shown is read off the event stream as each
//! commit lands, and an answer to a gate goes to [`Harness::answer`], so a run that waits for
//! a person can be answered in this process or a later one.
//!
//! Ctrl-C while a run is going asks it to stop ([`Harness::stop`]): the tool that is running
//! is told, the rest of the step's calls are not run, and the run ends as stopped. A second
//! Ctrl-C leaves at once, and the run stays unfinished in the log for `corduroy continue` or
//! `corduroy stop`.

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex, PoisonError};

use corduroy_harness::core::{Event, ListenerId, Pattern, SessionId, Source};
use corduroy_harness::message::decode;
use corduroy_harness::{Answer, Harness, HarnessError, Outcome, Status};
use serde_json::Value;

/// Where the terminal writes: shared, because events are shown from inside the loop.
pub type Out = Arc<Mutex<dyn Write + Send>>;

/// Write `text` and a newline to `out`. A terminal that cannot be written to has nobody to
/// tell, so the error is dropped.
pub fn say(out: &Out, text: &str) {
    let mut out = out.lock().unwrap_or_else(PoisonError::into_inner);
    drop(writeln!(out, "{text}"));
    drop(out.flush());
}

/// At most `limit` characters of the first line of `text`, marked where it was cut.
fn short(text: &str, limit: usize) -> String {
    let line = text.lines().next().unwrap_or_default();
    let mut shown: String = line.chars().take(limit).collect();
    let more_lines = text.trim_end().lines().count().saturating_sub(1);
    if shown.chars().count() < line.chars().count() {
        shown.push('…');
    }
    if more_lines > 0 {
        shown = format!("{shown} (+{more_lines} lines)");
    }
    shown
}

fn field<'a>(payload: &'a Value, name: &str) -> &'a str {
    payload
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// What the person is told when the model stopped for `reason`, where there is something
/// for them to do about it.
fn stopped(reason: &str) -> Option<&'static str> {
    match reason {
        "limit" => Some(
            "  ! the answer was cut off at the most the model may write: say \"go on\" for the \
             rest, or raise `max_tokens` for its provider",
        ),
        "refused" => {
            Some("  ! the model declined to answer: say it another way, or ask another model")
        }
        _ => None,
    }
}

/// How `event` is shown, if it is shown at all.
#[must_use]
pub fn describe(event: &Event) -> Option<String> {
    let payload: Value = serde_json::from_str(&event.payload).unwrap_or(Value::Null);
    match event.topic.as_str() {
        "message" if event.source == Source::Agent => {
            let text = decode(&event.payload).ok()?.text_content();
            (!text.trim().is_empty()).then(|| text.trim().to_owned())
        }
        "run/stop" => Some("■ stopping".to_owned()),
        "tool/call" => {
            let input = payload
                .get("input")
                .map(Value::to_string)
                .unwrap_or_default();
            Some(format!(
                "▸ {} {}",
                field(&payload, "tool"),
                short(&input, 100)
            ))
        }
        "tool/result" => {
            let output = field(&payload, "output");
            if payload.get("ok") == Some(&Value::Bool(true)) {
                Some(format!("  ✓ {}", short(output, 100)))
            } else {
                Some(format!("  ✗ {}", short(output, 200)))
            }
        }
        "gate/decision" if field(&payload, "verdict") == "deny" => Some(format!(
            "  ✗ denied by {}: {}",
            field(&payload, "gate"),
            field(&payload, "reason")
        )),
        "capability/denied" => Some(format!(
            "  ! {} was refused `{}` on {}",
            field(&payload, "plugin"),
            field(&payload, "capability"),
            field(&payload, "resource")
        )),
        "plugin/log" => Some(format!(
            "  [{}] {}",
            event.source,
            short(field(&payload, "message"), 200)
        )),
        "plugin/failed" | "plugin/stopped" => Some(format!(
            "  ! plugin {} {}: {}",
            field(&payload, "plugin"),
            event.topic.trim_start_matches("plugin/"),
            field(&payload, "reason")
        )),
        "run/restart" => Some("  ↻ picking up a run that was cut short".to_owned()),
        "usage" => stopped(field(&payload, "stop")).map(str::to_owned),
        _ => None,
    }
}

/// Show what happens in `session`, in the sub-agents it starts, and to the plugins, on
/// `out` as it is committed. A sub-agent's lines are indented under its session's name.
pub fn watch(harness: &Harness, session: &SessionId, out: &Out) -> ListenerId {
    let session = session.clone();
    let below = format!("{session}/");
    let out = Arc::clone(out);
    harness.events().subscribe(Pattern::any(), move |event| {
        let Some(text) = describe(event) else {
            return;
        };
        match &event.session {
            None => say(&out, &text),
            Some(id) if *id == session => say(&out, &text),
            Some(id) if id.as_str().starts_with(&below) => {
                let indented = text.replace('\n', "\n    ");
                say(&out, &format!("  [{id}] {indented}"));
            }
            Some(_) => {}
        }
    })
}

/// Ask a person about each tool call `outcome` waits on, until the run no longer waits.
///
/// `y` allows a call; anything else refuses it, and any text other than `n` is the reason the
/// model is given. When the input ends the run is left waiting, in the log, for `corduroy
/// continue` in this process or another.
///
/// # Errors
///
/// As [`Harness::answer`].
pub async fn settle(
    harness: &Harness,
    mut outcome: Outcome,
    input: &mut (dyn BufRead + Send),
    out: &Out,
) -> Result<Outcome, HarnessError> {
    while let Status::Waiting { asks, .. } = &outcome.status {
        let Some(ask) = asks.first().cloned() else {
            break;
        };
        say(
            out,
            &format!(
                "? {} {}\n  {} asks: {}\n  allow? [y/N, or a reason to refuse]",
                ask.tool,
                short(&ask.input, 300),
                ask.gate,
                ask.reason
            ),
        );
        let mut line = String::new();
        if input.read_line(&mut line).unwrap_or(0) == 0 {
            // Nobody is there to answer. The run waits in the log for whoever is next.
            break;
        }
        let answer = match line.trim() {
            "y" | "Y" | "yes" => Answer::Allow,
            "" | "n" | "N" | "no" => Answer::Deny("the user declined".to_owned()),
            reason => Answer::Deny(reason.to_owned()),
        };
        let answered = harness.answer(&ask.session, &ask.call, answer);
        outcome = stoppable(harness, &ask.session, answered).await?;
    }
    Ok(outcome)
}

/// Await `run` of `session`, asking it to stop when the person presses Ctrl-C. A second
/// press stops waiting, leaving the run unfinished in the log.
///
/// # Errors
///
/// What `run` returns, [`HarnessError::Log`] if the stop could not be recorded, or
/// [`HarnessError::Run`] after a second Ctrl-C.
pub async fn stoppable(
    harness: &Harness,
    session: &SessionId,
    run: impl Future<Output = Result<Outcome, HarnessError>>,
) -> Result<Outcome, HarnessError> {
    let mut run = std::pin::pin!(run);
    let mut asked = false;
    loop {
        tokio::select! {
            outcome = &mut run => return outcome,
            pressed = tokio::signal::ctrl_c() => {
                // Without a way to hear Ctrl-C, the run is simply awaited.
                if pressed.is_err() {
                    return run.await;
                }
                if asked {
                    return Err(HarnessError::Run(
                        "interrupted; the run is unfinished: `corduroy continue` or `corduroy stop`"
                            .to_owned(),
                    ));
                }
                asked = true;
                harness.stop(session)?;
            }
        }
    }
}

/// Run `prompt` in `session` and answer what the gates ask. A run a crash cut short is
/// finished first.
///
/// # Errors
///
/// As [`Harness::run_in`].
pub async fn prompt(
    harness: &Harness,
    session: &SessionId,
    prompt: &str,
    input: &mut (dyn BufRead + Send),
    out: &Out,
) -> Result<Outcome, HarnessError> {
    let outcome = resume(harness, session, input, out).await?;
    if matches!(outcome.status, Status::Waiting { .. }) {
        return Ok(outcome);
    }
    let outcome = stoppable(harness, session, harness.run_in(session, prompt)).await?;
    settle(harness, outcome, input, out).await
}

/// Finish whatever `session` left unfinished: answer a run that waits for a person, and
/// continue one a crash cut short.
///
/// # Errors
///
/// As [`Harness::continue_run`].
pub async fn resume(
    harness: &Harness,
    session: &SessionId,
    input: &mut (dyn BufRead + Send),
    out: &Out,
) -> Result<Outcome, HarnessError> {
    let outcome = stoppable(harness, session, harness.continue_run(session)).await?;
    settle(harness, outcome, input, out).await
}

/// One line about how a run ended, if the events have not already said it.
#[must_use]
pub fn ending(status: &Status) -> Option<String> {
    match status {
        Status::Finished { .. } | Status::Yielded => None,
        Status::Stopped => Some("stopped".to_owned()),
        Status::TurnLimitReached => Some("stopped: the turn limit was reached".to_owned()),
        Status::Failed { reason } => Some(format!("the run failed: {reason}")),
        Status::Waiting { asks, waits } if asks.is_empty() => {
            let signals: Vec<&str> = waits.iter().map(|wait| wait.signal.as_str()).collect();
            Some(format!("waiting for {}", signals.join(", ")))
        }
        Status::Waiting { asks, .. } => Some(format!(
            "waiting for {} answer(s); run `corduroy continue` to give them",
            asks.len()
        )),
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use corduroy_harness::core::{Draft, EventStream};

    use super::*;

    fn shown(source: Source, topic: &str, payload: &str) -> Option<String> {
        let events = EventStream::new();
        events
            .emit(Draft::new(source, topic).payload(payload))
            .unwrap();
        describe(&events.events()[0])
    }

    #[test]
    fn tool_calls_and_results_are_one_line_each() {
        let call = shown(
            Source::Agent,
            "tool/call",
            r#"{"id":"1","tool":"bash","input":{"command":"ls"}}"#,
        );
        assert_eq!(call.as_deref(), Some(r#"▸ bash {"command":"ls"}"#));
        let ok = shown(
            Source::Environment,
            "tool/result",
            r#"{"id":"1","tool":"bash","ok":true,"output":"a\nb\nc"}"#,
        );
        assert_eq!(ok.as_deref(), Some("  ✓ a (+2 lines)"));
        let failed = shown(
            Source::Environment,
            "tool/result",
            r#"{"id":"1","tool":"read","ok":false,"output":"denied"}"#,
        );
        assert_eq!(failed.as_deref(), Some("  ✗ denied"));
    }

    #[test]
    fn what_the_model_says_is_shown_and_its_tool_calls_are_not_repeated() {
        let said = r#"{"role":"assistant","parts":[{"type":"text","text":"Done."},{"type":"tool_call","id":"1","name":"ls","input":{}}]}"#;
        assert_eq!(
            shown(Source::Agent, "message", said).as_deref(),
            Some("Done.")
        );
        let calls_only = r#"{"role":"assistant","parts":[{"type":"tool_call","id":"1","name":"ls","input":{}}]}"#;
        assert_eq!(shown(Source::Agent, "message", calls_only), None);
        assert_eq!(
            shown(Source::User, "run/stop", "{}").as_deref(),
            Some("■ stopping")
        );
    }

    #[test]
    fn an_answer_cut_off_or_declined_says_what_the_person_can_do() {
        let cut = shown(Source::Agent, "usage", r#"{"model":"m","stop":"limit"}"#).unwrap();
        assert!(
            cut.contains("cut off") && cut.contains("max_tokens"),
            "{cut}"
        );
        let declined = shown(Source::Agent, "usage", r#"{"stop":"refused"}"#).unwrap();
        assert!(declined.contains("declined"), "{declined}");
    }

    #[test]
    fn quiet_events_are_not_shown() {
        assert_eq!(shown(Source::Host, "turn/end", ""), None);
        assert_eq!(shown(Source::Agent, "usage", "{}"), None);
        let finished = r#"{"model":"m","stop":"finished"}"#;
        assert_eq!(shown(Source::Agent, "usage", finished), None);
        let allowed = r#"{"verdict":"allow","gate":"approval","reason":""}"#;
        assert_eq!(shown(Source::Host, "gate/decision", allowed), None);
    }
}
