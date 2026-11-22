//! Stopping a run: while a tool runs, while the model is asked, while it waits for a person,
//! after a crash, and with a sub-agent under it. Native plugins only: no WebAssembly,
//! network or LLM.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use corduroy_harness::core::{
    Gate, Message, Pattern, SERVICES, Services, SessionId, TOOL_CALL, TOOLS, ToolError, ToolSpec,
    Tools, Verdict, plugin_fn, tool_fn,
};
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::{
    Harness, MAIN, ModelReply, ModelRequest, Provider, ProviderError, SESSIONS, Status,
};
use serde_json::{Value, json};
use tokio::sync::Notify;

/// The longest a test lets a stopped run take to end.
const PROMPTLY: Duration = Duration::from_secs(5);

/// A harness around `model` with three tools: `slow`, which tells `started` it began and
/// then works until its run is asked to stop; `quick`, which answers at once; and
/// `delegate`, which hands `{"task"}` to a child session.
fn harness(model: Arc<dyn Provider>, started: &Arc<Notify>) -> Harness {
    let harness = Harness::new(model).unwrap();
    let started = Arc::clone(started);
    harness.plugin(plugin_fn("tools", &[TOOLS, SERVICES], move |ctx| {
        let tools = ctx.service::<Tools>(TOOLS)?;
        let spec = |name: &str| ToolSpec::new(name, "", r#"{"type":"object"}"#);
        let begun = Arc::clone(&started);
        let slow = tool_fn(spec("slow"), move |_| {
            begun.notify_one();
            work_until_stopped()
        });
        tools.register(ctx, "slow", Arc::new(slow))?;
        tools.register(
            ctx,
            "quick",
            Arc::new(tool_fn(spec("quick"), |_| Ok("done"))),
        )?;
        let services = ctx.service::<Services>(SERVICES)?;
        let delegate = tool_fn(spec("delegate"), move |input| {
            services
                .call("tools", SESSIONS, "start", input)
                .map_err(|error| ToolError::Failed(error.to_string()))
        });
        tools.register(ctx, "delegate", Arc::new(delegate))?;
        Ok(())
    }));
    harness
}

/// What `slow` does: work until its run is asked to stop, or give up after [`PROMPTLY`].
fn work_until_stopped() -> Result<String, ToolError> {
    let until = Instant::now() + PROMPTLY;
    while !corduroy_harness::core::call::stopping() {
        if Instant::now() > until {
            return Err(ToolError::Failed("never told to stop".into()));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok("stopped early, with half the work done".to_owned())
}

/// A model that never answers, and says when it has been asked.
struct Silent(Arc<Notify>);

#[async_trait]
impl Provider for Silent {
    async fn complete(&self, _request: &ModelRequest) -> Result<ModelReply, ProviderError> {
        self.0.notify_one();
        std::future::pending().await
    }
}

/// The payload of each event on `topic`, parsed.
fn payloads(harness: &Harness, topic: &str) -> Vec<Value> {
    harness
        .events()
        .matching(&Pattern::new(topic))
        .iter()
        .filter_map(|event| serde_json::from_str(&event.payload).ok())
        .collect()
}

/// What each tool call returned, as `(id, ok, output)`.
fn results(harness: &Harness) -> Vec<(String, bool, String)> {
    payloads(harness, "tool/result")
        .into_iter()
        .map(|result| {
            (
                result["id"].as_str().unwrap_or_default().to_owned(),
                result["ok"].as_bool().unwrap_or_default(),
                result["output"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_running_tool_is_told_and_the_calls_after_it_are_not_run() {
    let started = Arc::new(Notify::new());
    let mut both = call("c1", "slow", &json!({}));
    both.parts.extend(call("c2", "quick", &json!({})).parts);
    let model = Arc::new(ScriptedProvider::new([both, say("picked up again")]));
    let harness = harness(Arc::<ScriptedProvider>::clone(&model), &started);
    let main = SessionId::new(MAIN);
    let begun = Instant::now();

    let stop = async {
        started.notified().await;
        harness.stop(&main).unwrap()
    };
    let (outcome, stopped) = tokio::join!(harness.run("work on it"), stop);

    assert!(stopped);
    assert_eq!(outcome.unwrap().status, Status::Stopped);
    assert!(begun.elapsed() < PROMPTLY);
    assert_eq!(
        results(&harness),
        [
            (
                "c1".to_owned(),
                true,
                "stopped early, with half the work done".to_owned()
            ),
            (
                "c2".to_owned(),
                false,
                "not run: the run was stopped before this call".to_owned()
            ),
        ]
    );
    // The stop and the end are both in the log.
    assert_eq!(payloads(&harness, "run/stop").len(), 1);
    assert_eq!(
        payloads(&harness, "turn/end"),
        [json!({"status": "stopped"})]
    );

    // The session takes a new prompt, and the model hears what happened.
    let next = harness.run("go on").await.unwrap();
    assert_eq!(
        next.status,
        Status::Finished {
            answer: "picked up again".into()
        }
    );
    let seen = &model.requests()[1].conversation;
    let told = seen.iter().flat_map(Message::tool_results).count();
    assert_eq!(told, 2);
}

#[tokio::test]
async fn a_model_that_does_not_answer_is_not_waited_for() {
    let asked = Arc::new(Notify::new());
    let harness = harness(
        Arc::new(Silent(Arc::clone(&asked))),
        &Arc::new(Notify::new()),
    );
    let main = SessionId::new(MAIN);

    let stop = async {
        asked.notified().await;
        harness.stop(&main).unwrap()
    };
    let (outcome, stopped) = tokio::join!(harness.run("hello?"), stop);

    assert!(stopped);
    assert_eq!(outcome.unwrap().status, Status::Stopped);
    // Nothing the model never said is in the conversation.
    assert_eq!(payloads(&harness, "message").len(), 1);
}

#[tokio::test]
async fn a_run_waiting_for_a_person_is_ended_and_its_call_is_not_run() {
    let model = Arc::new(ScriptedProvider::new([call("c1", "quick", &json!({}))]));
    let harness = harness(model, &Arc::new(Notify::new()));
    let ask: Arc<dyn Gate> =
        Arc::new(|point: &str, _: &str| (point == TOOL_CALL).then(|| Verdict::Ask("sure?".into())));
    harness.gates().insert("asker", ask).unwrap();
    let main = SessionId::new(MAIN);
    let waiting = harness.run("do it").await.unwrap();
    assert!(matches!(waiting.status, Status::Waiting { .. }));

    assert!(harness.stop(&main).unwrap());

    let journal = harness.journal(&main).unwrap();
    assert_eq!(journal.waiting().len(), 0);
    assert!(!journal.unfinished());
    assert_eq!(
        results(&harness),
        [(
            "c1".to_owned(),
            false,
            "not run: the run was stopped before this call".to_owned()
        )]
    );
    // Stopping again finds nothing to stop.
    assert!(!harness.stop(&main).unwrap());
}

#[tokio::test]
async fn a_run_a_crash_cut_short_can_be_ended_rather_than_continued() {
    let model = Arc::new(ScriptedProvider::new([say("fresh start")]));
    let harness = harness(model, &Arc::new(Notify::new()));
    let main = SessionId::new(MAIN);
    // A prompt recorded and never run: as a crash before the first step leaves it.
    let journal = harness.journal(&main).unwrap();
    journal.record(&Message::user().text("lost")).unwrap();
    assert!(harness.run("new").await.is_err());

    assert!(harness.stop(&main).unwrap());

    let outcome = harness.run("new").await.unwrap();
    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "fresh start".into()
        }
    );
}

#[tokio::test]
async fn stopping_a_run_stops_the_sub_agent_it_waits_for() {
    let started = Arc::new(Notify::new());
    let model = Arc::new(ScriptedProvider::new([
        call("p1", "delegate", &json!({"task": "the long part"})),
        call("k1", "slow", &json!({})),
    ]));
    let harness = harness(model, &started);
    let main = SessionId::new(MAIN);

    let stop = async {
        started.notified().await;
        harness.stop(&main).unwrap()
    };
    let (outcome, stopped) = tokio::join!(harness.run("delegate it"), stop);

    assert!(stopped);
    assert_eq!(outcome.unwrap().status, Status::Stopped);
    let child = SessionId::new("main/1");
    let ends: Vec<(Option<String>, Value)> = harness
        .events()
        .matching(&Pattern::new("turn/end"))
        .iter()
        .map(|event| {
            let session = event.session.as_ref().map(|id| id.as_str().to_owned());
            (session, serde_json::from_str(&event.payload).unwrap())
        })
        .collect();
    assert_eq!(
        ends,
        [
            (
                Some(child.as_str().to_owned()),
                json!({"status": "stopped"})
            ),
            (Some(MAIN.to_owned()), json!({"status": "stopped"})),
        ]
    );
    // The parent's call was answered with what the sub-agent's end brought.
    let parent = harness.journal(&main).unwrap().session();
    let answered: Vec<String> = parent
        .conversation()
        .iter()
        .flat_map(Message::tool_results)
        .map(|result| result.output.to_text())
        .collect();
    assert_eq!(answered, ["The sub-agent was stopped before it finished."]);
}
