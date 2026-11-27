//! Gates deciding whether the model's tool calls run, and runs that wait for a person.
//! No WebAssembly, network or LLM.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use corduroy_harness::core::{
    EventStream, GATES, Gate, Gates, Pattern, Plugin, SessionId, TOOL_CALL, TOOLS, ToolSpec, Tools,
    Verdict, plugin_fn, tool_fn,
};
use corduroy_harness::core::{Message, ToolCall};
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::{Answer, FRONTS, Harness, HarnessError, MAIN, OUTSIDE, Outside, Status};
use corduroy_jsonl as jsonl;
use corduroy_test_support::scratch;
use serde_json::{Value, json};

/// `peek`, which only looks, and `poke` and `launch`, which change things. Each counts the
/// times it ran in `runs`.
fn toolbox(runs: &Arc<AtomicUsize>) -> impl Plugin + 'static {
    let runs = Arc::clone(runs);
    plugin_fn("toolbox", &[TOOLS], move |ctx| {
        let tools = ctx.service::<Tools>(TOOLS)?;
        for (name, read_only) in [("peek", true), ("poke", false), ("launch", false)] {
            let spec = ToolSpec::new(name, "", r#"{"type":"object"}"#)
                .replay_safe(read_only)
                .read_only(read_only);
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
async fn an_answer_recorded_from_outside_is_taken_on_by_the_host() {
    let runs = Arc::new(AtomicUsize::new(0));
    let replies = [call("c1", "poke", &json!({})), say("poked")];
    let harness = harness(EventStream::new(), replies, &runs).unwrap();
    let main = SessionId::new(MAIN);
    // What an extension is handed, by name.
    let outside = harness.app().service::<Outside>(OUTSIDE).unwrap();
    harness.run("poke it").await.unwrap();
    assert_eq!(outside.asks().len(), 1);
    assert_eq!(outside.asks()[0].call, "c1");

    outside.answer("page", &main, "c1", &Answer::Allow).unwrap();

    // Recorded, with who it came through, and nothing run: that is the host's to do.
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    let recorded = &payloads(&harness, "gate/answer")[0];
    assert_eq!(
        (&recorded["allow"], &recorded["via"]),
        (&json!(true), &json!("page"))
    );
    assert_eq!(outside.asks(), []);
    // The question is answered once: a second answer, from anywhere, finds nothing waiting.
    let again = outside.answer("page", &main, "c1", &Answer::Deny("no".into()));
    assert!(matches!(again, Err(HarnessError::NotAsked { .. })));
    let typed = harness.answer(&main, "c1", Answer::Allow).await;
    assert!(matches!(typed, Err(HarnessError::NotAsked { .. })));

    let outcome = harness.take_on(&main).await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "poked".into()
        }
    );
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(payloads(&harness, "gate/answer").len(), 1);
    assert_eq!(payloads(&harness, "run/restart"), Vec::<Value>::new());
    // Taking on a run that has ended runs nothing more.
    let asked = payloads(&harness, "model/request").len();
    let ended = harness.take_on(&main).await.unwrap();
    assert_eq!(ended.status, outcome.status);
    assert_eq!(payloads(&harness, "model/request").len(), asked);
}

#[tokio::test]
async fn a_run_is_not_taken_on_while_another_of_its_questions_waits() {
    let runs = Arc::new(AtomicUsize::new(0));
    let both = Message::assistant()
        .call(ToolCall::new("c1", "poke", "{}"))
        .call(ToolCall::new("c2", "poke", "{}"));
    let harness = harness(EventStream::new(), [both, say("poked twice")], &runs).unwrap();
    let main = SessionId::new(MAIN);
    let outside = harness.app().service::<Outside>(OUTSIDE).unwrap();
    harness.run("poke it twice").await.unwrap();

    outside
        .answer("page", &main, "c2", &Answer::Deny("not that one".into()))
        .unwrap();
    let waiting = harness.take_on(&main).await.unwrap();

    let Status::Waiting { asks, .. } = &waiting.status else {
        panic!("expected to wait, got {:?}", waiting.status);
    };
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0].call, "c1");
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    // The other answered at the terminal: the run goes on, one call made and one refused.
    let outcome = harness.answer(&main, "c1", Answer::Allow).await.unwrap();
    assert!(matches!(outcome.status, Status::Finished { .. }));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_message_said_from_outside_begins_a_run_the_host_takes_on() {
    let runs = Arc::new(AtomicUsize::new(0));
    let replies = [say("hello"), call("c1", "poke", &json!({})), say("poked")];
    let harness = harness(EventStream::new(), replies, &runs).unwrap();
    let main = SessionId::new(MAIN);
    let outside = harness.app().service::<Outside>(OUTSIDE).unwrap();

    // A host that runs one prompt and leaves would record it and run it never.
    let unheard = outside.say("page", &main, "say hello").unwrap_err();
    assert!(unheard.to_string().contains("nobody is here"), "{unheard}");
    harness.takes_messages(true);
    assert!(outside.say("page", &main, "  ").is_err());
    assert!(
        outside
            .say("page", &SessionId::new("nowhere"), "hi")
            .is_err()
    );
    assert_eq!(payloads(&harness, "message"), Vec::<Value>::new());

    outside.say("page", &main, "say hello").unwrap();

    // Recorded as the person's, with what it came through, and nothing asked of the model.
    let said = &payloads(&harness, "message")[0];
    assert_eq!(
        (&said["role"], &said["via"]),
        (&json!("user"), &json!("page"))
    );
    assert_eq!(payloads(&harness, "model/request"), Vec::<Value>::new());
    // A run is under way from that moment: a second message has to wait for its end.
    let busy = outside.say("page", &main, "and again");
    assert!(matches!(busy, Err(HarnessError::Unfinished(_))), "{busy:?}");

    let outcome = harness.take_on(&main).await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "hello".into()
        }
    );
    assert_eq!(payloads(&harness, "run/restart"), Vec::<Value>::new());
    // While a question waits, the session takes no message either.
    outside.say("page", &main, "poke it").unwrap();
    harness.take_on(&main).await.unwrap();
    let waiting = outside.say("page", &main, "hurry");
    assert!(
        matches!(waiting, Err(HarnessError::Waiting(_))),
        "{waiting:?}"
    );
}

#[tokio::test]
async fn a_stop_asked_from_outside_ends_a_run_that_waits() {
    let runs = Arc::new(AtomicUsize::new(0));
    let replies = [call("c1", "poke", &json!({})), say("after the stop")];
    let harness = harness(EventStream::new(), replies, &runs).unwrap();
    let main = SessionId::new(MAIN);
    let outside = harness.app().service::<Outside>(OUTSIDE).unwrap();
    assert!(!outside.stop("page", &main).unwrap());
    harness.run("poke it").await.unwrap();

    assert!(outside.stop("page", &main).unwrap());

    // Ended as stopped, by the record alone: the call never ran, and nothing waits.
    assert_eq!(payloads(&harness, "run/stop")[0]["via"], json!("page"));
    assert_eq!(
        payloads(&harness, "turn/end")[0]["status"],
        json!("stopped")
    );
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert_eq!(outside.asks(), []);
    let outcome = harness.take_on(&main).await.unwrap();
    assert_eq!(outcome.status, Status::Stopped);
    assert!(!outside.stop("page", &main).unwrap());
    // The session takes a new prompt.
    let next = harness.run("and now?").await.unwrap();
    assert!(matches!(next.status, Status::Finished { .. }));
}

#[test]
fn what_acts_from_outside_is_out_of_reach_of_add_ons() {
    let runs = Arc::new(AtomicUsize::new(0));
    let harness = harness(EventStream::new(), [], &runs).unwrap();
    // An add-on reaches services through the `Services` registry alone, and neither is
    // there: a gate's question is the person's to answer.
    assert!(harness.services().get(OUTSIDE).is_none());
    assert!(harness.services().get(FRONTS).is_none());
    assert_eq!(harness.fronts().names(), Vec::<String>::new());
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
        both = both.call(ToolCall::new(id, tool, "{}"));
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
