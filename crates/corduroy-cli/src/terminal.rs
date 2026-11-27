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
use std::sync::{Arc, Mutex, PoisonError, Weak, mpsc};
use std::thread;
use std::time::Duration;

use corduroy_harness::core::{Event, ListenerId, Pattern, SessionId, Source};
use corduroy_harness::message::decode;
use corduroy_harness::{
    Answer, Ask, Harness, HarnessError, Outcome, Piece, Seen, Status, Watch as Pieces,
};
use serde_json::Value;
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// Where the terminal writes: shared, because events are shown from inside the loop.
pub type Out = Arc<Mutex<dyn Write + Send>>;

/// Write `text` and a newline to `out`. A terminal that cannot be written to has nobody to
/// tell, so the error is dropped.
pub fn say(out: &Out, text: &str) {
    let mut out = out.lock().unwrap_or_else(PoisonError::into_inner);
    drop(writeln!(out, "{text}"));
    drop(out.flush());
}

/// Read a line of `input` each time one is `wanted` and hand it over, `None` for the end of
/// the input, until nobody wants or takes any more.
fn read_lines(
    input: &mut dyn BufRead,
    wanted: &mpsc::Receiver<()>,
    read: &UnboundedSender<Option<String>>,
) {
    while wanted.recv().is_ok() {
        let mut line = String::new();
        let more = input.read_line(&mut line).unwrap_or(0) > 0;
        if read.send(more.then_some(line)).is_err() {
            return;
        }
    }
}

/// What a person types, a line at a time.
///
/// The lines are read on a thread of their own, so that waiting for one can be given up:
/// a question put at the terminal may be answered somewhere else first. A line that was
/// waited for and then no longer wanted is kept, and is the next one handed out.
#[derive(Debug)]
pub struct Lines {
    /// Asks the thread for one more line.
    want: mpsc::Sender<()>,
    /// The lines it read, `None` for the end of the input.
    typed: UnboundedReceiver<Option<String>>,
    /// Whether a line was asked for that has not been handed out yet.
    asked: bool,
}

impl Lines {
    /// The lines of `input`, read only as they are asked for.
    #[must_use]
    pub fn new(mut input: Box<dyn BufRead + Send>) -> Self {
        let (want, wanted) = mpsc::channel::<()>();
        let (read, typed) = unbounded_channel();
        let reader = move || read_lines(&mut *input, &wanted, &read);
        // Without a thread nobody can type: every line asked for is the end of the input.
        drop(
            thread::Builder::new()
                .name("corduroy-input".to_owned())
                .spawn(reader),
        );
        Self {
            want,
            typed,
            asked: false,
        }
    }

    /// The next line typed, or `None` once the input has ended. Giving up the wait loses
    /// nothing: the line, when it comes, is the next one returned.
    pub async fn next(&mut self) -> Option<String> {
        if !self.asked {
            self.want.send(()).ok()?;
            self.asked = true;
        }
        let line = self.typed.recv().await.flatten();
        self.asked = false;
        line
    }
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

/// How an answer or a stop given at a front end is shown. One typed here needs no line.
fn given_elsewhere(topic: &str, payload: &Value) -> Option<String> {
    let via = field(payload, "via");
    if via.is_empty() {
        return None;
    }
    match topic {
        "gate/answer" if payload.get("allow") == Some(&Value::Bool(true)) => {
            Some(format!("  ✓ allowed on {via}"))
        }
        "gate/answer" => Some(format!(
            "  ✗ refused on {via}: {}",
            field(payload, "reason")
        )),
        "run/stop" => Some(format!("■ stopping, as asked on {via}")),
        _ => None,
    }
}

/// How `event` is shown, if it is shown at all.
#[must_use]
pub fn describe(event: &Event) -> Option<String> {
    let payload: Value = serde_json::from_str(&event.payload).unwrap_or(Value::Null);
    if let Some(given) = given_elsewhere(&event.topic, &payload) {
        return Some(given);
    }
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
        // A call of the agent's that a gate denied gets a `tool/result` saying so, which is
        // shown; a plugin's own call and a load get none, so the decision is their line.
        "gate/decision"
            if field(&payload, "verdict") == "deny" && field(&payload, "caller") != "agent" =>
        {
            Some(format!(
                "  ✗ denied by {}: {}",
                field(&payload, "gate"),
                field(&payload, "reason")
            ))
        }
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

/// Whether `event` is an answer of the agent's: something it said without asking for a
/// tool. What it says on the way to calling one is progress.
fn answers(event: &Event) -> bool {
    event.topic == "message"
        && event.source == Source::Agent
        && decode(&event.payload).is_ok_and(|message| message.tool_calls().next().is_none())
}

/// Where the line being written stands, for what the model writes shown as it writes.
#[derive(Debug, Default)]
struct Line {
    /// Text was written and its line not ended.
    open: bool,
    /// How much of the answer now being written has been shown.
    text: Told,
    /// That the model is thinking has been said for this answer.
    thinking: bool,
}

/// How much of an answer's text was shown while it was written.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Told {
    /// None of it: the whole is shown when it is committed.
    #[default]
    Nothing,
    /// All there was.
    All,
    /// Some, with some missed on the way: the whole is shown when it is committed.
    Some,
}

impl Told {
    /// What was shown, once more of the text has been.
    const fn with_more(self) -> Self {
        match self {
            Self::Nothing | Self::All => Self::All,
            Self::Some => Self::Some,
        }
    }
}

/// What the model of one session writes, written to the terminal as it comes.
///
/// Read in two places, both under `line`: by a thread of its own, so text appears while
/// the model writes, and by the listener on the event stream before it shows an event, so
/// an answer's last pieces are on the terminal before the line its commit would add.
struct Streamed {
    pieces: Pieces,
    out: Out,
    line: Mutex<Line>,
}

impl Streamed {
    /// Write `text` as it is, with no line end of its own.
    fn write(&self, text: &str) {
        let mut out = self.out.lock().unwrap_or_else(PoisonError::into_inner);
        drop(write!(out, "{text}"));
        drop(out.flush());
    }

    /// End the line being written, if one is.
    fn end(&self, line: &mut Line) {
        if line.open {
            self.write("\n");
            line.open = false;
        }
    }

    /// End the line being written, if one is, taking the lock to look.
    fn end_now(&self) {
        let mut line = self.line.lock().unwrap_or_else(PoisonError::into_inner);
        self.end(&mut line);
    }

    /// The request is made again: say so if any of its answer was shown, and begin anew.
    fn start_over(&self, line: &mut Line) {
        self.end(line);
        if line.text != Told::Nothing {
            say(
                &self.out,
                "  ↻ the model is asked again: the answer starts over",
            );
        }
        *line = Line::default();
    }

    /// Write whatever the model has written since the last look.
    fn catch_up(&self, line: &mut Line) {
        for shown in self.pieces.take() {
            match shown.seen {
                Seen::Piece(Piece::Text(text)) if !text.is_empty() => {
                    self.write(&text);
                    line.open = !text.ends_with('\n');
                    line.text = line.text.with_more();
                }
                // Said once and quietly: what it thinks is in the log for whoever wants it.
                Seen::Piece(Piece::Thinking(_)) if !line.thinking => {
                    self.end(line);
                    say(&self.out, "  … thinking");
                    line.thinking = true;
                }
                Seen::Restarted => self.start_over(line),
                Seen::Missed => line.text = Told::Some,
                _ => {}
            }
        }
    }

    /// Show `event` of the session's own, described as `text`, after whatever was written
    /// before it. An answer whose text was shown as it came is not said twice.
    fn show(&self, event: &Event, text: Option<&str>) {
        let mut line = self.line.lock().unwrap_or_else(PoisonError::into_inner);
        self.catch_up(&mut line);
        let message = event.topic == "message" && event.source == Source::Agent;
        let shown_already = message && line.text == Told::All;
        // A run that ends while the model writes leaves a line unfinished too.
        let over = message || event.topic == "turn/end";
        if text.is_some() || over {
            self.end(&mut line);
        }
        if let Some(text) = text
            && !shown_already
        {
            say(&self.out, text);
        }
        if over {
            *line = Line::default();
        }
    }
}

/// Keep writing what `streamed` is shown, for as long as anything else holds it.
fn follow(streamed: &Weak<Streamed>) {
    while let Some(streamed) = streamed.upgrade() {
        if streamed.pieces.pending(Duration::from_millis(100)) {
            let mut line = streamed.line.lock().unwrap_or_else(PoisonError::into_inner);
            streamed.catch_up(&mut line);
        }
    }
}

/// Show what happens in `session`, in the sub-agents it starts, and to the plugins, as it
/// is committed.
///
/// What the session's agent answers is written to `answers`, and everything else to `out`,
/// which may be the same. A sub-agent's lines are indented under its session's name.
///
/// With `live`, what the session's model writes is also written as it writes it (the
/// harness's [live channel](corduroy_harness::live)), to `out`, and an answer shown that
/// way is not written again when it is committed. That is for a person reading at a
/// terminal where answers and progress go to the same place: a program that reads the
/// answers is given each whole, once.
pub fn watch(
    harness: &Harness,
    session: &SessionId,
    answers: &Out,
    out: &Out,
    live: bool,
) -> ListenerId {
    let streamed = live.then(|| {
        let streamed = Arc::new(Streamed {
            pieces: harness.watch(Some(session)),
            out: Arc::clone(out),
            line: Mutex::default(),
        });
        let followed = Arc::downgrade(&streamed);
        // Without the thread, text still appears: at each event, and so at the latest when
        // its message is committed.
        drop(
            thread::Builder::new()
                .name("corduroy-live".to_owned())
                .spawn(move || follow(&followed)),
        );
        streamed
    });
    let session = session.clone();
    let below = format!("{session}/");
    let (answered, out) = (Arc::clone(answers), Arc::clone(out));
    harness.events().subscribe(Pattern::any(), move |event| {
        let text = describe(event);
        if let Some(streamed) = &streamed
            && event.session.as_ref().is_none_or(|id| *id == session)
        {
            streamed.show(event, text.as_deref());
            return;
        }
        let Some(text) = text else {
            return;
        };
        match &event.session {
            None => say(&out, &text),
            Some(id) if *id == session && self::answers(event) => say(&answered, &text),
            Some(id) if *id == session => say(&out, &text),
            Some(id) if id.as_str().starts_with(&below) => {
                if let Some(streamed) = &streamed {
                    streamed.end_now();
                }
                let indented = text.replace('\n', "\n    ");
                say(&out, &format!("  [{id}] {indented}"));
            }
            Some(_) => {}
        }
    })
}

/// Who answered a question put at the terminal.
enum Heard {
    /// The person here, with this line.
    Typed(String),
    /// Someone at a front end: the answer is in the log already.
    Elsewhere,
    /// Nobody, and nobody will: the input has ended.
    Nobody,
}

/// Whether `ask` still waits for an answer.
fn waits(harness: &Harness, ask: &Ask) -> bool {
    harness.journal(&ask.session).is_ok_and(|journal| {
        journal
            .waiting()
            .iter()
            .any(|waiting| waiting.call == ask.call)
    })
}

/// Wait for an answer to `ask`: a line typed here, or an answer recorded from a front end,
/// of which `answered` tells.
async fn hear(harness: &Harness, ask: &Ask, input: &mut Lines, answered: &Notify) -> Heard {
    loop {
        if !waits(harness, ask) {
            return Heard::Elsewhere;
        }
        tokio::select! {
            line = input.next() => return line.map_or(Heard::Nobody, Heard::Typed),
            // Some question was answered, or a run ended: look again whether this one
            // still waits.
            () = answered.notified() => {}
        }
    }
}

/// Ask a person about each tool call `outcome` waits on, until the run no longer waits.
///
/// `y` allows a call; anything else refuses it, and any text other than `n` is the reason the
/// model is given. A line that begins `@<id>` is for the question about that tool call
/// alone: it is given to that one, and to none if no such question waits. A question
/// answered at a front end meanwhile (the agent's page, say) is
/// answered: the run is taken on from there, and what was being typed goes to the next
/// question. When the input ends the run is left waiting, in the log, for `corduroy
/// continue` in this process or another.
///
/// # Errors
///
/// As [`Harness::answer`].
pub async fn settle(
    harness: &Harness,
    outcome: Outcome,
    input: &mut Lines,
    out: &Out,
) -> Result<Outcome, HarnessError> {
    // An answer, or the end of the run, which a stop asked for at a front end brings and
    // which takes the run's questions with it. Not the stop itself: it is recorded before
    // the run has ended, when the question still waits.
    let answered = Arc::new(Notify::new());
    let listeners = ["gate/answer", "turn/end"].map(|topic| {
        let heard = Arc::clone(&answered);
        harness
            .events()
            .subscribe(topic, move |_| heard.notify_one())
    });
    let settled = ask_each(harness, outcome, input, out, &answered).await;
    for listener in listeners {
        harness.events().unsubscribe(listener);
    }
    settled
}

/// What a line typed at a question says: whether the call is allowed, and which question
/// the line is for, if it says.
///
/// `y` allows; anything else refuses, and any text other than `n` is the reason the model
/// is given. A line that begins `@<id>` answers the question about the tool call `<id>`
/// and no other, so that an answer prepared for one question is never taken for the next.
fn answer_of(line: &str) -> (Option<&str>, Answer) {
    let line = line.trim();
    let (id, said) = line.strip_prefix('@').map_or((None, line), |addressed| {
        let (id, said) = addressed
            .split_once(char::is_whitespace)
            .unwrap_or((addressed, ""));
        (Some(id), said.trim())
    });
    let answer = match said {
        "y" | "Y" | "yes" => Answer::Allow,
        "" | "n" | "N" | "no" => Answer::Deny("the user declined".to_owned()),
        reason => Answer::Deny(reason.to_owned()),
    };
    (id, answer)
}

async fn ask_each(
    harness: &Harness,
    mut outcome: Outcome,
    input: &mut Lines,
    out: &Out,
    answered: &Notify,
) -> Result<Outcome, HarnessError> {
    while let Status::Waiting { asks, .. } = &outcome.status {
        let Some(ask) = asks.first().cloned() else {
            break;
        };
        say(
            out,
            &format!(
                "? {} {}\n  {} asks ({}): {}\n  allow? [y/N, or a reason to refuse]",
                ask.tool,
                short(&ask.input, 300),
                ask.gate,
                ask.call,
                ask.reason
            ),
        );
        let typed = match hear(harness, &ask, input, answered).await {
            Heard::Typed(line) => Some(line),
            Heard::Elsewhere => None,
            // Nobody is there to answer. The run waits in the log for whoever is next.
            Heard::Nobody => break,
        };
        // An answer that names a question is given to that one, wherever it stands among
        // those waiting, and to none if no such question waits.
        let answer = match typed.as_deref().map(answer_of) {
            None => None,
            Some((None, answer)) => Some((ask.clone(), answer)),
            Some((Some(id), answer)) => {
                let Some(named) = asks.iter().find(|ask| ask.call == id) else {
                    say(
                        out,
                        &format!("  ! no question `{id}` is waiting: nothing was answered"),
                    );
                    continue;
                };
                Some((named.clone(), answer))
            }
        };
        let session = answer
            .as_ref()
            .map_or_else(|| ask.session.clone(), |(asked, _)| asked.session.clone());
        let session = &session;
        let typed = match answer {
            Some((asked, answer)) => {
                let run = harness.answer(session, &asked.call, answer);
                Some(stoppable(harness, session, run).await)
            }
            None => None,
        };
        outcome = match typed {
            Some(Ok(outcome)) => outcome,
            // Answered at a front end, perhaps in the moment the line was typed, or the
            // run was stopped there: it is taken on from what the log holds.
            None | Some(Err(HarnessError::NotAsked { .. })) => {
                stoppable(harness, session, harness.take_on(session)).await?
            }
            Some(Err(error)) => return Err(error),
        };
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
    input: &mut Lines,
    out: &Out,
) -> Result<Outcome, HarnessError> {
    let outcome = resume(harness, session, input, out).await?;
    if matches!(outcome.status, Status::Waiting { .. }) {
        return Ok(outcome);
    }
    let outcome = stoppable(harness, session, harness.run_in(session, prompt)).await?;
    settle(harness, outcome, input, out).await
}

/// Something said to a session at a front end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The session it was said to.
    pub session: SessionId,
    /// What was said.
    pub text: String,
    /// The plugin it was given through.
    pub via: String,
}

/// What is said to sessions from outside the terminal, at a front end, as it is recorded.
///
/// Such a message is in the log already, and the run it began waits for the host to take it
/// on ([`taken_on`]). The terminal listens while it waits for the person to type.
#[derive(Debug)]
pub struct Said {
    listener: ListenerId,
    told: Arc<Notify>,
    waiting: Arc<Mutex<Vec<Message>>>,
}

impl Said {
    /// Listen on `harness` for what is said from outside, and tell it that such messages
    /// are taken on here ([`Harness::takes_messages`]).
    #[must_use]
    pub fn listen(harness: &Harness) -> Self {
        let told = Arc::new(Notify::new());
        let waiting = Arc::new(Mutex::new(Vec::new()));
        let (tell, kept) = (Arc::clone(&told), Arc::clone(&waiting));
        let listener = harness.events().subscribe("message", move |event| {
            let payload: Value = serde_json::from_str(&event.payload).unwrap_or(Value::Null);
            // A message typed here names nothing it came through.
            let (Some(session), via) = (&event.session, field(&payload, "via")) else {
                return;
            };
            if event.source != Source::User || via.is_empty() {
                return;
            }
            let message = Message {
                session: session.clone(),
                text: decode(&event.payload)
                    .map_or_else(|_| String::new(), |said| said.text_content()),
                via: via.to_owned(),
            };
            kept.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(message);
            tell.notify_one();
        });
        harness.takes_messages(true);
        Self {
            listener,
            told,
            waiting,
        }
    }

    /// The next thing said from outside, waiting for one if there is none.
    pub async fn next(&self) -> Message {
        loop {
            let first = {
                let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
                (!waiting.is_empty()).then(|| waiting.remove(0))
            };
            if let Some(message) = first {
                return message;
            }
            self.told.notified().await;
        }
    }

    /// Stop listening: `harness` takes no more messages from outside.
    pub fn close(self, harness: &Harness) {
        harness.takes_messages(false);
        harness.events().unsubscribe(self.listener);
    }
}

/// Take on the run of `session` that something recorded from outside began or let go on (a
/// message said at a front end, say), and answer what the gates ask.
///
/// # Errors
///
/// As [`Harness::take_on`].
pub async fn taken_on(
    harness: &Harness,
    session: &SessionId,
    input: &mut Lines,
    out: &Out,
) -> Result<Outcome, HarnessError> {
    let outcome = stoppable(harness, session, harness.take_on(session)).await?;
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
    input: &mut Lines,
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
        Status::CutOff { .. } => Some(
            "cut off: the answer stopped at the most the model may write, and is not whole"
                .to_owned(),
        ),
        Status::TurnLimitReached => Some("stopped: the turn limit was reached".to_owned()),
        Status::TimeLimitReached => Some("stopped: the time limit was reached".to_owned()),
        Status::OutputLimitReached => {
            Some("stopped: the model had written all it may in one run".to_owned())
        }
        Status::Failed { reason } => Some(format!("the run failed: {}", plain(reason))),
        Status::Waiting { asks, waits } if asks.is_empty() => {
            let signals: Vec<&str> = waits.iter().map(|wait| wait.signal.as_str()).collect();
            Some(format!("waiting for {}", signals.join(", ")))
        }
        Status::Waiting { asks, .. } => {
            let ids: Vec<&str> = asks.iter().map(|ask| ask.call.as_str()).collect();
            Some(format!(
                "waiting for {} answer(s), about {}; run `corduroy continue` to give them",
                asks.len(),
                ids.join(", ")
            ))
        }
        other => Some(other.to_string()),
    }
}

/// `reason`, why a run failed, as a person reads it.
///
/// A service's own answer often arrives quoted whole, as JSON
/// (`… API error: {"message": "…", "code": "…"}`); its message and its code are put in its
/// place. Anything else is left as it is, and the log keeps the reason as it came.
#[must_use]
pub fn plain(reason: &str) -> String {
    let quoted = reason
        .find('{')
        .zip(reason.rfind('}'))
        .and_then(|(from, to)| Some((reason.get(..from)?, reason.get(from..=to)?)))
        .and_then(|(before, json)| Some((before, serde_json::from_str::<Value>(json).ok()?)));
    let Some((before, answer)) = quoted else {
        return reason.to_owned();
    };
    // The answer itself, or the `error` it wraps one in.
    let said = answer.get("error").unwrap_or(&answer);
    let Some(message) = said.get("message").and_then(Value::as_str) else {
        return reason.to_owned();
    };
    let code = ["code", "type"]
        .iter()
        .find_map(|key| said.get(key).and_then(Value::as_str))
        .map(|code| format!(" ({code})"))
        .unwrap_or_default();
    let before = before.trim_end().trim_end_matches(':');
    let lead = if before.is_empty() {
        String::new()
    } else {
        format!("{before}: ")
    };
    format!("{lead}{}{code}", message.trim())
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
    fn a_service_answer_quoted_as_json_is_said_as_its_message_and_code() {
        let limit = "Request failed: Responses API error: {\"message\":\"The ChatGPT user has \
                     reached their Subscription Sharing usage limit.\",\"type\":\
                     \"invalid_request_error\",\"param\":null,\"code\":\
                     \"subscription_sharing_usage_limit_exceeded\"}";
        assert_eq!(
            plain(limit),
            "Request failed: Responses API error: The ChatGPT user has reached their \
             Subscription Sharing usage limit. (subscription_sharing_usage_limit_exceeded)"
        );
        // An answer that wraps its error, with a type and no code.
        let wrapped =
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        assert_eq!(plain(wrapped), "Overloaded (overloaded_error)");
        // What quotes no answer, or one with no message, stays as it came.
        for reason in [
            "Server error (500) at http://127.0.0.1:1/v1: overloaded",
            "bad input {\"detail\": 3}",
            "a brace { left open",
            "",
        ] {
            assert_eq!(plain(reason), reason);
        }
        let failed = Status::Failed {
            reason: wrapped.to_owned(),
        };
        assert_eq!(
            ending(&failed).as_deref(),
            Some("the run failed: Overloaded (overloaded_error)")
        );
    }

    #[test]
    fn a_typed_answer_may_name_its_question() {
        let refused = |reason: &str| Answer::Deny(reason.to_owned());
        assert_eq!(answer_of("y\n"), (None, Answer::Allow));
        assert_eq!(answer_of("\n"), (None, refused("the user declined")));
        assert_eq!(answer_of("use b.txt"), (None, refused("use b.txt")));
        assert_eq!(answer_of("@call_1 y\n"), (Some("call_1"), Answer::Allow));
        assert_eq!(
            answer_of("@call_1 no, use b.txt"),
            (Some("call_1"), refused("no, use b.txt"))
        );
        // Named and nothing said: refused, as an empty line is.
        assert_eq!(
            answer_of("@call_1"),
            (Some("call_1"), refused("the user declined"))
        );
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

    #[test]
    fn a_denial_is_said_once() {
        // The agent's call: the result that follows says who denied it and why.
        let agents = r#"{"point":"tool/call","id":"1","tool":"bash","verdict":"deny","gate":"approval","reason":"no shell","caller":"agent"}"#;
        assert_eq!(shown(Source::Host, "gate/decision", agents), None);
        let result =
            r#"{"id":"1","tool":"bash","ok":false,"output":"denied by gate `approval`: no shell"}"#;
        assert_eq!(
            shown(Source::Environment, "tool/result", result).as_deref(),
            Some("  ✗ denied by gate `approval`: no shell")
        );
        // A plugin's call has no result on the stream, so the decision is shown.
        let plugins = r#"{"point":"tool/call","id":"1","tool":"bash","verdict":"deny","gate":"approval","reason":"no shell","caller":"plugin:todo"}"#;
        assert_eq!(
            shown(Source::Host, "gate/decision", plugins).as_deref(),
            Some("  ✗ denied by approval: no shell")
        );
    }

    /// What a run of `harness` writes on a terminal that shows the model's text live.
    async fn written_live(harness: &Harness, prompt: &str) -> (Status, String) {
        let written = Arc::new(Mutex::new(Vec::new()));
        let out: Out = Arc::<Mutex<Vec<u8>>>::clone(&written);
        let main = SessionId::new(corduroy_harness::MAIN);
        let shown = watch(harness, &main, &out, &out, true);
        let outcome = harness.run(prompt).await.unwrap();
        harness.events().unsubscribe(shown);
        let text = String::from_utf8(written.lock().unwrap().clone()).unwrap();
        (outcome.status, text)
    }

    #[tokio::test]
    async fn an_answer_shown_as_it_is_written_is_not_said_again() {
        use corduroy_harness::scripted::{ScriptedProvider, call, say as answer};
        let mut first = answer("Looking at it.");
        first
            .parts
            .extend(call("c1", "missing", &serde_json::json!({})).parts);
        let model = ScriptedProvider::new([first, answer("one two\nthree")]).piecewise(3);
        let harness = Harness::new(Arc::new(model)).unwrap();

        let (status, text) = written_live(&harness, "count").await;

        assert!(matches!(status, Status::Finished { .. }), "{status:?}");
        // Each thing the model said is there once, its line ended before the next begins.
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.first(), Some(&"Looking at it."), "{text}");
        assert!(
            lines
                .get(1)
                .is_some_and(|line| line.starts_with("▸ missing")),
            "{text}"
        );
        assert_eq!(lines.get(3..), Some(&["one two", "three"][..]), "{text}");
        assert!(text.ends_with("three\n"), "{text:?}");
    }

    #[tokio::test]
    async fn a_run_ended_while_the_model_writes_ends_its_line() {
        use corduroy_harness::scripted::ScriptedProvider;
        let pieces = [
            Piece::Thinking("hm".to_owned()),
            Piece::Thinking("mm".to_owned()),
            Piece::Text("so f".to_owned()),
            Piece::Text("ar".to_owned()),
        ];
        let harness = Harness::new(Arc::new(ScriptedProvider::new([]).hanging(pieces)))
            .unwrap()
            .with_time_limit(Duration::from_millis(100));

        let (status, text) = written_live(&harness, "go").await;

        assert_eq!(status, Status::TimeLimitReached);
        assert_eq!(text, "  … thinking\nso far\n");
    }
}
