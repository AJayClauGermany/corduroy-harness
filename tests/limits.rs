//! What ends a run besides its turns: the time it is driven for, what the model writes in
//! it, and an answer the model could not finish. Native plugins only; no network or LLM.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use corduroy_harness::core::{
    Context, EventStream, Message, Pattern, PluginError, TOOLS, ToolCall, ToolSpec, Tools,
    plugin_fn, tool_fn,
};
use corduroy_harness::{
    Harness, HarnessError, MAIN, ModelReply, ModelRequest, Provider, ProviderError, Status,
    StopReason, Usage,
};
use serde_json::json;

/// A model that gives each of its replies in turn, saying what it wrote and why it stopped.
struct Counted {
    replies: Mutex<VecDeque<ModelReply>>,
    asked: Mutex<usize>,
}

impl Counted {
    fn new(replies: impl IntoIterator<Item = ModelReply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into_iter().collect()),
            asked: Mutex::new(0),
        })
    }

    fn asked(&self) -> usize {
        *self.asked.lock().unwrap()
    }
}

#[async_trait]
impl Provider for Counted {
    async fn complete(&self, _request: &ModelRequest) -> Result<ModelReply, ProviderError> {
        *self.asked.lock().unwrap() += 1;
        let reply = self.replies.lock().unwrap().pop_front();
        reply.ok_or_else(|| ProviderError::new("the script ran out"))
    }
}

/// A model that never answers.
struct Silent;

#[async_trait]
impl Provider for Silent {
    async fn complete(&self, _request: &ModelRequest) -> Result<ModelReply, ProviderError> {
        std::future::pending().await
    }
}

/// A reply calling `noop`, for which the model wrote `output` tokens.
fn working(id: &str, output: u64) -> ModelReply {
    let message = Message::assistant().call(ToolCall::new(id, "noop", "{}"));
    ModelReply::new(message, Usage::new(Some(10), Some(output)))
}

/// A reply saying `text`, for which the model wrote `output` tokens.
fn saying(text: &str, output: u64) -> ModelReply {
    ModelReply::new(
        Message::assistant().text(text),
        Usage::new(Some(10), Some(output)),
    )
}

fn noop(ctx: &mut Context<'_>) -> Result<(), PluginError> {
    let tools = ctx.service::<Tools>(TOOLS)?;
    let spec = ToolSpec::new(
        "noop",
        "Does nothing.",
        json!({"type": "object"}).to_string(),
    );
    let tool = tool_fn(spec, |_input| Ok(String::from("done")));
    tools.register(ctx, "noop", Arc::new(tool))?;
    Ok(())
}

fn harness(provider: Arc<dyn Provider>) -> Result<Harness, HarnessError> {
    let harness = Harness::new(provider)?;
    harness.plugin(plugin_fn("noop", &[TOOLS], noop));
    Ok(harness)
}

fn ends(harness: &Harness) -> Vec<String> {
    harness
        .events()
        .matching(&Pattern::new("turn/end"))
        .into_iter()
        .map(|event| event.payload)
        .collect()
}

#[tokio::test]
async fn a_run_that_has_written_all_it_may_is_not_asked_again() {
    let model = Counted::new([working("c1", 60), working("c2", 60), working("c3", 60)]);
    let harness = harness(Arc::<Counted>::clone(&model))
        .unwrap()
        .with_output_limit(100);

    let outcome = harness.run("spin").await.unwrap();

    // The request that passed the limit was answered whole and its tool was called.
    assert_eq!(outcome.status, Status::OutputLimitReached);
    assert_eq!(model.asked(), 2);
    assert_eq!(outcome.session.turn_output_tokens(), 120);
    assert_eq!(outcome.session.pending_calls(), vec![]);
    // The end says what the run's two requests used in all.
    assert_eq!(
        ends(&harness),
        [
            r#"{"status":"output-limit","used":{"requests":2,"input_tokens":20,"output_tokens":120}}"#
        ]
    );
}

#[tokio::test]
async fn what_the_model_wrote_is_counted_from_the_last_prompt_and_read_from_the_log() {
    let model = Counted::new([saying("one", 80), saying("two", 80), saying("three", 80)]);
    let harness = harness(Arc::<Counted>::clone(&model))
        .unwrap()
        .with_output_limit(100);

    // Each prompt begins a run of its own, so three answers of 80 tokens all fit.
    for prompt in ["a", "b"] {
        let outcome = harness.run(prompt).await.unwrap();
        assert!(matches!(outcome.status, Status::Finished { .. }));
        assert_eq!(outcome.session.turn_output_tokens(), 80);
    }

    // A harness opened on the same log counts the same.
    let events = Arc::new(EventStream::load(harness.events().events()));
    let resumed = Harness::resume(model, events).unwrap();
    let session = resumed.journal(&MAIN.into()).unwrap().session();
    assert_eq!(session.turn_output_tokens(), 80);
}

#[tokio::test]
async fn a_run_out_of_time_is_not_asked_again() {
    let model = Counted::new([working("c1", 1), working("c2", 1)]);
    let harness = harness(Arc::<Counted>::clone(&model))
        .unwrap()
        .with_time_limit(Duration::ZERO);

    let outcome = harness.run("spin").await.unwrap();

    assert_eq!(outcome.status, Status::TimeLimitReached);
    assert_eq!(model.asked(), 0);
    assert_eq!(ends(&harness), [r#"{"status":"time-limit"}"#]);
}

#[tokio::test]
async fn a_request_still_being_answered_when_the_time_is_up_is_dropped() {
    let harness = harness(Arc::new(Silent))
        .unwrap()
        .with_time_limit(Duration::from_millis(100));
    let began = Instant::now();

    let outcome = harness.run("anyone there?").await.unwrap();

    assert_eq!(outcome.status, Status::TimeLimitReached);
    assert!(began.elapsed() >= Duration::from_millis(100));
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "{:?}",
        began.elapsed()
    );
    // The request is in the log with no answer, and the run has its end.
    let asked = harness.events().matching(&Pattern::new("model/request"));
    assert_eq!(asked.len(), 1);
    assert_eq!(ends(&harness), [r#"{"status":"time-limit"}"#]);

    // Taken on again, the run has its whole time again: here, long enough for an answer.
    let model = Counted::new([saying("here", 1)]);
    let events = Arc::new(EventStream::load(harness.events().events()));
    let patient = Harness::resume(model, events)
        .unwrap()
        .with_time_limit(Duration::from_secs(60));
    let outcome = patient.run("and now?").await.unwrap();
    assert!(matches!(outcome.status, Status::Finished { .. }));
}

#[tokio::test]
async fn an_answer_cut_off_at_the_models_limit_is_not_a_finished_run() {
    let cut = saying("The first half of", 4096).stopped(StopReason::Limit);
    let whole = saying("the whole of it.", 20).stopped(StopReason::Finished);
    let model = Counted::new([cut, whole]);
    let harness = harness(model).unwrap();

    let outcome = harness.run("write it all").await.unwrap();

    let Status::CutOff { answer } = &outcome.status else {
        panic!("expected a cut-off answer, got {:?}", outcome.status);
    };
    assert_eq!(answer, "The first half of");
    assert_eq!(
        ends(&harness),
        [r#"{"status":"cut-off","used":{"requests":1,"input_tokens":10,"output_tokens":4096}}"#]
    );

    // The next answer is whole, and so the run that asked for it finishes.
    let outcome = harness.run("go on").await.unwrap();
    assert!(matches!(outcome.status, Status::Finished { .. }));
}

#[tokio::test]
async fn a_cut_off_call_for_a_tool_is_still_run() {
    // Cut off or not, a message that asks for a tool is no answer: the run goes on.
    let cut = working("c1", 4096).stopped(StopReason::Limit);
    let model = Counted::new([cut, saying("done", 5)]);
    let harness = harness(model).unwrap();

    let outcome = harness.run("do it").await.unwrap();

    assert!(matches!(outcome.status, Status::Finished { .. }));
}

#[tokio::test]
async fn a_log_that_says_finished_reads_as_finished() {
    // Logs written before a cut-off answer had an ending of its own say `finished`.
    let model = Counted::new([saying("half", 9).stopped(StopReason::Limit)]);
    let harness = harness(Arc::<Counted>::clone(&model)).unwrap();
    harness.run("write").await.unwrap();
    let events: Vec<_> = harness
        .events()
        .events()
        .into_iter()
        .map(|mut event| {
            if event.topic == "turn/end" {
                event.payload = r#"{"status":"finished"}"#.to_owned();
            }
            event
        })
        .collect();

    let old = Harness::resume(model, Arc::new(EventStream::load(events))).unwrap();
    let outcome = old.continue_run(&MAIN.into()).await.unwrap();

    assert!(matches!(outcome.status, Status::Finished { .. }));
}

/// What a run's requests used is added up on its `turn/end` (a prototype: G17 in
/// `docs/open-questions.md`). The total is the same whether the run went through in one
/// process or was picked up from the log by another, and each run has its own.
#[tokio::test]
async fn the_end_of_a_run_says_what_its_requests_used_in_all() {
    let total = |end: &str| {
        let end: serde_json::Value = serde_json::from_str(end).unwrap();
        end["used"].clone()
    };
    let model = Counted::new([working("c1", 7), saying("done", 5), saying("again", 3)]);
    let whole = harness(model).unwrap();
    whole.run("work").await.unwrap();
    whole.run("more").await.unwrap();

    let ended = ends(&whole);
    assert_eq!(
        total(&ended[0]),
        json!({"requests": 2, "input_tokens": 20, "output_tokens": 12})
    );
    // The next run counts from nothing.
    assert_eq!(
        total(&ended[1]),
        json!({"requests": 1, "input_tokens": 10, "output_tokens": 3})
    );

    // A process that stopped after the first request left its `usage` in the log; the one
    // that picks the run up adds its own request to it.
    let all = whole.events().events();
    let first_answer = all.iter().position(|event| event.topic == "usage").unwrap();
    let survived = EventStream::load(all.into_iter().take(first_answer + 1));
    let picked_up = Harness::resume(Counted::new([saying("done", 5)]), Arc::new(survived)).unwrap();
    picked_up.plugin(plugin_fn("noop", &[TOOLS], noop));
    picked_up.continue_run(&MAIN.into()).await.unwrap();
    assert_eq!(total(&ends(&picked_up)[0]), total(&ended[0]));
}
