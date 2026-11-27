//! An answer as the model writes it: the pieces shown to whoever watches, the checkpoints
//! in the log, and what a request that never answered leaves. No network or LLM.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use corduroy_harness::core::{EventStream, Message, Pattern, SessionId};
use corduroy_harness::live::WATCH_BYTES;
use corduroy_harness::scripted::{ScriptedProvider, say};
use corduroy_harness::{
    CallPiece, Checkpoints, Harness, MAIN, ModelReply, ModelRequest, Piece, Provider,
    ProviderError, ProviderErrorKind, Reports, Retry, Seen, Shown, Status, Usage,
};
use serde_json::Value;

/// A harness over `events` that asks `model`, checkpointing as `checkpoints` says.
fn harness(
    events: &Arc<EventStream>,
    model: Arc<dyn Provider>,
    checkpoints: Checkpoints,
) -> Harness {
    Harness::resume(model, Arc::clone(events))
        .unwrap()
        .with_checkpoints(checkpoints)
}

/// The payload of each event of `topic`.
fn recorded(events: &EventStream, topic: &str) -> Vec<Value> {
    events
        .matching(&Pattern::new(topic))
        .iter()
        .map(|event| serde_json::from_str(&event.payload).unwrap())
        .collect()
}

/// The text among `shown`, put together.
fn text_of(shown: &[Shown]) -> String {
    shown
        .iter()
        .filter_map(|shown| match &shown.seen {
            Seen::Piece(Piece::Text(text)) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// What the model was told and said in the main session, as text.
fn conversation(harness: &Harness) -> Vec<String> {
    let session = harness.journal(&SessionId::new(MAIN)).unwrap().session();
    session
        .conversation()
        .iter()
        .map(Message::text_content)
        .collect()
}

#[tokio::test]
async fn pieces_reach_a_watcher_in_order_and_the_answer_is_committed_whole() {
    let events = Arc::new(EventStream::new());
    let model = ScriptedProvider::new([say("one two three four")]).piecewise(4);
    let harness = harness(&events, Arc::new(model), Checkpoints::Every(Duration::ZERO));
    let main = SessionId::new(MAIN);
    let watch = harness.watch(Some(&main));
    let elsewhere = harness.watch(Some(&SessionId::new("another")));

    let outcome = harness.run("count").await.unwrap();

    assert!(
        matches!(&outcome.status, Status::Finished { answer } if answer == "one two three four")
    );
    let shown = watch.take();
    assert!(shown.iter().all(|shown| shown.session == main));
    assert_eq!(text_of(&shown), "one two three four");
    assert_eq!(elsewhere.take(), []);
    // With a checkpoint for every piece, each holds what came since the one before.
    let checkpoints = recorded(&events, "model/progress");
    assert_eq!(checkpoints.len(), 5, "{checkpoints:?}");
    let kept: String = checkpoints
        .iter()
        .filter_map(|checkpoint| checkpoint["text"].as_str())
        .collect();
    assert_eq!(kept, "one two three four");
    let last = checkpoints.last().unwrap();
    assert_eq!(last["tokens"], 6);
    assert_eq!(last["estimated"], true);
    assert_eq!(last["model"], "default");
    // The answer is one message all the same, and nothing was left unanswered.
    assert_eq!(conversation(&harness), ["count", "one two three four"]);
    assert_eq!(recorded(&events, "model/partial"), Vec::<Value>::new());
}

/// A model that answers only whole, as one that does not stream.
struct Whole;

#[async_trait]
impl Provider for Whole {
    async fn complete(&self, _request: &ModelRequest) -> Result<ModelReply, ProviderError> {
        let message = Message::assistant()
            .text("all at once")
            .call(corduroy_harness::core::ToolCall::new("c1", "missing", "{}"));
        Ok(ModelReply::new(message, Usage::default()))
    }
}

#[tokio::test]
async fn a_provider_that_does_not_stream_is_shown_whole_and_never_checkpointed() {
    let events = Arc::new(EventStream::new());
    let harness = harness(&events, Arc::new(Whole), Checkpoints::default()).with_max_turns(1);
    let watch = harness.watch(None);

    let outcome = harness.run("say it").await.unwrap();

    assert_eq!(outcome.status, Status::TurnLimitReached);
    let seen: Vec<Seen> = watch.take().into_iter().map(|shown| shown.seen).collect();
    assert_eq!(
        seen,
        [
            Seen::Piece(Piece::Text("all at once".to_owned())),
            Seen::Piece(Piece::Call(CallPiece::new("c1", "missing", "{}"))),
        ]
    );
    assert_eq!(recorded(&events, "model/progress"), Vec::<Value>::new());
    assert_eq!(recorded(&events, "model/partial"), Vec::<Value>::new());
}

#[tokio::test]
async fn a_watcher_that_never_reads_does_not_hold_the_run() {
    let events = Arc::new(EventStream::new());
    let long = "word ".repeat(WATCH_BYTES / 2);
    let model = ScriptedProvider::new([say(&long)]).piecewise(1000);
    let harness = harness(&events, Arc::new(model), Checkpoints::Off);
    let unread = harness.watch(None);

    let outcome = harness.run("go on at length").await.unwrap();

    assert!(matches!(outcome.status, Status::Finished { .. }));
    assert_eq!(recorded(&events, "model/progress"), Vec::<Value>::new());
    // The run went on; the watcher lost the oldest of what it had not read, and is told.
    let shown = unread.take();
    assert_eq!(shown.first().map(|shown| &shown.seen), Some(&Seen::Missed));
    let kept = text_of(&shown);
    assert!(kept.len() <= WATCH_BYTES, "{}", kept.len());
    assert!(long.ends_with(&kept));
}

/// The pieces a model wrote before it stopped answering.
fn half_written() -> Vec<Piece> {
    vec![
        Piece::Thinking("let me see. ".to_owned()),
        Piece::Text("The answer ".to_owned()),
        Piece::Text("is ".to_owned()),
        Piece::Call(CallPiece::new("c1", "bash", "{\"command\":")),
        Piece::Call(CallPiece::new("c1", "", "\"rm -rf")),
    ]
}

#[tokio::test]
async fn a_stop_while_the_model_writes_keeps_what_it_had_written() {
    let events = Arc::new(EventStream::new());
    let model = ScriptedProvider::new([]).hanging(half_written());
    let harness = harness(&events, Arc::new(model), Checkpoints::Off);
    let main = SessionId::new(MAIN);
    let watch = harness.watch(Some(&main));

    let stop = async {
        // Stopped once the last piece has been shown.
        let mut seen = Vec::new();
        while !seen.iter().any(|shown: &Shown| {
            matches!(&shown.seen, Seen::Piece(Piece::Call(call)) if call.name.is_empty())
        }) {
            seen.extend(watch.next().await);
        }
        harness.stop(&main).unwrap()
    };
    let (outcome, stopped) = tokio::join!(harness.run("what is it?"), stop);

    assert!(stopped);
    assert_eq!(outcome.unwrap().status, Status::Stopped);
    let partials = recorded(&events, "model/partial");
    assert_eq!(partials.len(), 1, "{partials:?}");
    let partial = &partials[0];
    assert_eq!(partial["why"], "stopped");
    assert_eq!(partial["kind"], Value::Null);
    assert_eq!(partial["text"], "The answer is ");
    assert_eq!(partial["thinking"], "let me see. ");
    assert_eq!(
        partial["calls"],
        serde_json::json!([{"id": "c1", "tool": "bash", "input": "{\"command\":\"rm -rf"}])
    );
    // It is evidence for a reader and nothing else: the conversation does not hold it, the
    // call in it was never asked about or run, and a log read back says the same.
    assert_eq!(conversation(&harness), ["what is it?"]);
    for topic in ["tool/call", "tool/start", "gate/decision"] {
        assert_eq!(recorded(&events, topic), Vec::<Value>::new(), "{topic}");
    }
    let again = Harness::resume(Arc::new(ScriptedProvider::new([])), Arc::clone(&events)).unwrap();
    assert_eq!(conversation(&again), ["what is it?"]);
    assert!(!again.journal(&main).unwrap().unfinished());
}

#[tokio::test]
async fn a_request_the_time_limit_ends_keeps_what_it_had_written() {
    let events = Arc::new(EventStream::new());
    let model = ScriptedProvider::new([]).hanging([Piece::Text("so far".to_owned())]);
    let harness = harness(&events, Arc::new(model), Checkpoints::default())
        .with_time_limit(Duration::from_millis(100));

    let outcome = harness.run("take your time").await.unwrap();

    assert_eq!(outcome.status, Status::TimeLimitReached);
    let partials = recorded(&events, "model/partial");
    assert_eq!(partials.len(), 1, "{partials:?}");
    assert_eq!(partials[0]["why"], "time-limit");
    assert_eq!(partials[0]["text"], "so far");
    assert!(partials[0]["elapsed_ms"].as_u64().unwrap() >= 100);
}

/// A model whose first attempt writes something and fails, is tried again, and then
/// either answers or fails for good.
struct Faltering {
    asked: AtomicUsize,
    recovers: bool,
}

#[async_trait]
impl Provider for Faltering {
    async fn complete(&self, request: &ModelRequest) -> Result<ModelReply, ProviderError> {
        self.complete_reporting(request, &()).await
    }

    async fn complete_reporting(
        &self,
        _request: &ModelRequest,
        reports: &dyn Reports,
    ) -> Result<ModelReply, ProviderError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        reports.piece(&Piece::Text("a false st".to_owned()));
        let lost = ProviderError::new("connection lost").of(ProviderErrorKind::Unreachable);
        reports.retry(&Retry::new(1, lost.clone(), Duration::ZERO));
        reports.piece(&Piece::Text("second ".to_owned()));
        if !self.recovers {
            return Err(lost);
        }
        reports.piece(&Piece::Text("try".to_owned()));
        Ok(ModelReply::new(
            Message::assistant().text("second try"),
            Usage::default(),
        ))
    }
}

#[tokio::test]
async fn a_retry_voids_what_the_failed_attempt_wrote() {
    let events = Arc::new(EventStream::new());
    let model = Arc::new(Faltering {
        asked: AtomicUsize::new(0),
        recovers: true,
    });
    let harness = harness(&events, model, Checkpoints::Every(Duration::ZERO));
    let watch = harness.watch(None);

    let outcome = harness.run("try").await.unwrap();

    assert!(matches!(&outcome.status, Status::Finished { answer } if answer == "second try"));
    // The watcher is told the first attempt's text no longer counts.
    let seen: Vec<Seen> = watch.take().into_iter().map(|shown| shown.seen).collect();
    assert_eq!(
        seen,
        [
            Seen::Piece(Piece::Text("a false st".to_owned())),
            Seen::Restarted,
            Seen::Piece(Piece::Text("second try".to_owned())),
        ]
    );
    assert_eq!(recorded(&events, "model/retry").len(), 1);
    assert_eq!(recorded(&events, "model/partial"), Vec::<Value>::new());

    // And a request that fails for good keeps only what its last attempt wrote.
    let events = Arc::new(EventStream::new());
    let model = Arc::new(Faltering {
        asked: AtomicUsize::new(0),
        recovers: false,
    });
    let harness = self::harness(&events, model, Checkpoints::Off);
    let outcome = harness.run("try").await.unwrap();
    assert!(matches!(outcome.status, Status::Failed { .. }));
    let partials = recorded(&events, "model/partial");
    assert_eq!(partials.len(), 1, "{partials:?}");
    assert_eq!(partials[0]["why"], "failed");
    assert_eq!(partials[0]["kind"], "unreachable");
    assert_eq!(partials[0]["text"], "second ");
}
