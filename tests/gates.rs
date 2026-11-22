//! Gates deciding whether the model's tool calls run, and runs that wait for a person.
//! No WebAssembly, network or LLM.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use corduroy_harness::core::{
    EventStream, GATES, Gate, Gates, Pattern, Plugin, SessionId, TOOL_CALL, TOOLS, ToolSpec, Tools,
    Verdict, plugin_fn, tool_fn,
};
use corduroy_harness::oi_kit_provider_types::conversation::message::Message;
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::{Answer, Harness, HarnessError, MAIN, Status, jsonl};
use corduroy_test_support::scratch;
use rmcp::model::CallToolRequestParams;
use serde_json::{Value, json};

/// `peek`, which only looks, and `poke` and `launch`, which change things. Each counts the
/// times it ran in `runs`.
fn toolbox(runs: &Arc<AtomicUsize>) -> impl Plugin + 'static {
    let runs = Arc::clone(runs);
    plugin_fn("toolbox", &[TOOLS], move |ctx| {
        let tools = ctx.service::<Tools>(TOOLS)?;
        for (name, read_only) in [("peek", true), ("poke", false), ("launch", false)] {
            let spec = ToolSpec {
                name: name.into(),
                description: String::new(),
                input_schema: r#"{"type":"object"}"#.into(),
                replay_safe: read_only,
                read_only,
            };
            let runs = Arc::clone(&runs);
            let tool = tool_fn(spec, move |_| {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(format!("{name} done"))
            });
            tools.register(ctx, name, Arc::new(tool))?;
        }
        Ok(())
    })
}

/// A gate that denies `launch`, lets read-only calls through and asks about the rest.
fn policy() -> impl Plugin + 'static {
    plugin_fn("policy", &[GATES], |ctx| {
        let gate: Arc<dyn Gate> = Arc::new(|point: &str, request: &str| {
            if point != TOOL_CALL {
                return None;
            }
            let request: Value = serde_json::from_str(request).ok()?;
            Some(if request["tool"] == "launch" {
                Verdict::Deny("launching is not allowed here".into())
            } else if request["read-only"] == true {
                Verdict::Allow
            } else {
                Verdict::Ask(format!("{} changes things", request["tool"]))
            })
        });
        ctx.service::<Gates>(GATES)?.register(ctx, "policy", gate)?;
        Ok(())
    })
}

fn harness(
    events: EventStream,
    replies: impl IntoIterator<Item = Message>,
    runs: &Arc<AtomicUsize>,
) -> Result<Harness, HarnessError> {
    let model = Arc::new(ScriptedProvider::new(replies));
    let harness = Harness::resume(model, Arc::new(events))?;
    harness.plugin(toolbox(runs));
    harness.plugin(policy());
    Ok(harness)
}

fn payloads(harness: &Harness, topic: &str) -> Vec<Value> {
    harness
        .events()
        .matching(&Pattern::new(topic))
        .iter()
        .filter_map(|event| serde_json::from_str(&event.payload).ok())
        .collect()
}

#[tokio::test]
async fn a_denied_call_never_runs_and_the_model_learns_why() {
    let runs = Arc::new(AtomicUsize::new(0));
    let harness = harness(
        EventStream::new(),
        [
            call("c1", "launch", &json!({})),
            say("understood, no launch"),
        ],
        &runs,
    )
    .unwrap();

    let outcome = harness.run("launch it").await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "understood, no launch".into()
        }
    );
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    let decision = &payloads(&harness, "gate/decision")[0];
    assert_eq!(decision["verdict"], "deny");
    assert_eq!(decision["gate"], "policy");
    let result = &payloads(&harness, "tool/result")[0];
    assert_eq!(result["ok"], false);
    assert_eq!(
        result["output"],
        "denied by gate `policy`: launching is not allowed here"
    );
    assert_eq!(payloads(&harness, "tool/start"), Vec::<Value>::new());
}

#[tokio::test]
async fn an_allowed_call_runs_with_its_decision_on_record() {
    let runs = Arc::new(AtomicUsize::new(0));
    let harness = harness(
        EventStream::new(),
        [call("c1", "peek", &json!({})), say("peeked")],
        &runs,
    )
    .unwrap();

    harness.run("peek").await.unwrap();

    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let decisions = payloads(&harness, "gate/decision");
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0]["verdict"], "allow");
    // The decision is stored before the tool starts.
    let topics: Vec<String> = harness
        .events()
        .matching(&Pattern::new("*"))
        .into_iter()
        .map(|event| event.topic)
        .filter(|topic| topic.starts_with("gate/") || topic.starts_with("tool/"))
        .collect();
    assert_eq!(
        topics,
        ["tool/call", "gate/decision", "tool/start", "tool/result"]
    );
}

#[tokio::test]
async fn an_ask_waits_for_a_person_and_runs_once_allowed() {
    let runs = Arc::new(AtomicUsize::new(0));
    let harness = harness(
        EventStream::new(),
        [call("c1", "poke", &json!({"hard": true})), say("poked")],
        &runs,
    )
    .unwrap();
    let main = SessionId::new(MAIN);

    let outcome = harness.run("poke it").await.unwrap();

    let Status::Waiting { asks, .. } = &outcome.status else {
        panic!("expected to wait, got {:?}", outcome.status);
    };
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0].call, "c1");
    assert_eq!(asks[0].tool, "poke");
    assert_eq!(asks[0].input, r#"{"hard":true}"#);
    assert_eq!(asks[0].reason, r#""poke" changes things"#);
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    // A waiting run takes no new prompt, and continuing it does nothing until answered.
    assert!(matches!(
        harness.run("something else").await,
        Err(HarnessError::Waiting(_))
    ));
    let still = harness.continue_run(&main).await.unwrap();
    assert!(matches!(still.status, Status::Waiting { .. }));
    assert_eq!(payloads(&harness, "run/restart"), Vec::<Value>::new());

    let outcome = harness.answer(&main, "c1", Answer::Allow).await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "poked".into()
        }
    );
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(payloads(&harness, "gate/answer")[0]["allow"], true);
    // The gate was asked once, not again after the answer.
    assert_eq!(payloads(&harness, "gate/decision").len(), 1);
}

#[tokio::test]
async fn a_declined_ask_goes_back_to_the_model_with_the_reason() {
    let runs = Arc::new(AtomicUsize::new(0));
    let harness = harness(
        EventStream::new(),
        [call("c1", "poke", &json!({})), say("fine, I won't")],
        &runs,
    )
    .unwrap();
    let main = SessionId::new(MAIN);
    harness.run("poke it").await.unwrap();

    let outcome = harness
        .answer(&main, "c1", Answer::Deny("not today".into()))
        .await
        .unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "fine, I won't".into()
        }
    );
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert_eq!(
        payloads(&harness, "tool/result")[0]["output"],
        "a person declined this call: not today"
    );
}

#[tokio::test]
async fn a_message_waits_until_every_ask_in_it_is_answered() {
    let runs = Arc::new(AtomicUsize::new(0));
    let mut both = Message::assistant();
    for (id, tool) in [("c1", "poke"), ("c2", "peek"), ("c3", "poke")] {
        both = both.with_tool_request(id, Ok(CallToolRequestParams::new(tool.to_owned())));
    }
    let harness = harness(EventStream::new(), [both, say("all done")], &runs).unwrap();
    let main = SessionId::new(MAIN);
    harness.run("go").await.unwrap();

    let first = harness.answer(&main, "c1", Answer::Allow).await.unwrap();

    // `peek` was allowed but waits with the rest of its message.
    let Status::Waiting { asks, .. } = first.status else {
        panic!("expected to wait");
    };
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0].call, "c3");
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert!(matches!(
        harness.answer(&main, "c1", Answer::Allow).await,
        Err(HarnessError::NotAsked { .. })
    ));

    let done = harness
        .answer(&main, "c3", Answer::Deny("one is enough".into()))
        .await
        .unwrap();

    assert_eq!(
        done.status,
        Status::Finished {
            answer: "all done".into()
        }
    );
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_waiting_run_survives_the_process_and_is_answered_in_the_next() {
    let path = scratch("gates-survive").join("run.jsonl");
    let runs = Arc::new(AtomicUsize::new(0));
    let first = harness(
        jsonl::open(&path).unwrap(),
        [call("c1", "poke", &json!({}))],
        &runs,
    )
    .unwrap();
    first.run("poke it").await.unwrap();
    drop(first);

    let second = harness(jsonl::open(&path).unwrap(), [say("poked at last")], &runs).unwrap();
    let main = SessionId::new(MAIN);
    let waiting = second.continue_run(&main).await.unwrap();
    assert!(matches!(waiting.status, Status::Waiting { .. }));

    let outcome = second.answer(&main, "c1", Answer::Allow).await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "poked at last".into()
        }
    );
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn without_gates_nothing_is_decided_or_recorded() {
    let runs = Arc::new(AtomicUsize::new(0));
    let model = Arc::new(ScriptedProvider::new([
        call("c1", "launch", &json!({})),
        say("launched"),
    ]));
    let harness = Harness::new(model).unwrap();
    harness.plugin(toolbox(&runs));

    harness.run("launch").await.unwrap();

    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(payloads(&harness, "gate/*"), Vec::<Value>::new());
}
