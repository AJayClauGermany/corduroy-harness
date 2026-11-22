//! Compaction: a summary in place of the start of a long conversation. The harness decides
//! where to cut and records it; a plugin providing the `compaction` service decides when and
//! writes the summary. No network or LLM.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use corduroy_harness::core::Message;
use corduroy_harness::core::{
    CallError, EventStream, Pattern, Scope, Service, SessionId, TOOLS, ToolSpec, Tools, plugin_fn,
    tool_fn,
};
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::wasm::WasmRuntime;
use corduroy_harness::{COMPACTION, Harness, MAIN, Status, provider};
use corduroy_test_support::plugin;
use serde_json::{Value, json};

/// A harness whose model gives `replies`, with an `echo` tool.
fn harness(model: &Arc<ScriptedProvider>, events: Arc<EventStream>) -> Harness {
    let harness = Harness::resume(Arc::<ScriptedProvider>::clone(model), events).unwrap();
    harness.plugin(plugin_fn("echo", &[TOOLS], |ctx| {
        let spec = ToolSpec::new("echo", "", r#"{"type":"object"}"#)
            .replay_safe(true)
            .read_only(true);
        let tools = ctx.service::<Tools>(TOOLS)?;
        tools.register(
            ctx,
            "echo",
            Arc::new(tool_fn(spec, |input| Ok(input.to_owned()))),
        )?;
        Ok(())
    }));
    harness
}

/// A compaction service, due once two earlier messages are visible, that counts its
/// summaries.
fn service(summaries: &Arc<AtomicUsize>) -> Arc<dyn Service> {
    let summaries = Arc::clone(summaries);
    Arc::new(move |_caller: &str, method: &str, payload: &str| {
        let payload: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
        match method {
            "due" => Ok(json!({"due": payload["messages"].as_u64() >= Some(2)}).to_string()),
            "summarize" => {
                summaries.fetch_add(1, Ordering::SeqCst);
                let said = payload["transcript"].as_str().unwrap_or_default().len();
                Ok(json!({"summary": format!("SUMMARY of {said} characters")}).to_string())
            }
            other => Err(CallError::Unknown(other.to_owned())),
        }
    })
}

/// The text of each message the model would be sent.
fn seen_by_model(harness: &Harness) -> Vec<String> {
    let session = harness.journal(&SessionId::new(MAIN)).unwrap().session();
    session
        .for_model()
        .iter()
        .map(Message::text_content)
        .collect()
}

#[tokio::test]
async fn earlier_turns_give_way_to_a_summary_and_the_current_turn_is_kept_whole() {
    let model = Arc::new(ScriptedProvider::new([
        say("first answer"),
        call("1", "echo", &json!({"x": 1})),
        say("second answer"),
    ]));
    let events = Arc::new(EventStream::new());
    let harness = harness(&model, Arc::clone(&events));
    let summaries = Arc::new(AtomicUsize::new(0));
    harness
        .services()
        .insert(COMPACTION, service(&summaries))
        .unwrap();

    harness.run("first question").await.unwrap();
    // Two messages from before this turn are visible, so the step before the model's first
    // request of this turn condenses them.
    let outcome = harness.run("second question").await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "second answer".into()
        }
    );
    assert_eq!(
        summaries.load(Ordering::SeqCst),
        1,
        "compacted once, not every step"
    );
    let seen = seen_by_model(&harness);
    assert!(seen[0].contains("<summary>\nSUMMARY of"), "{seen:?}");
    assert_eq!(seen[1], "Understood. I will continue from that summary.");
    assert_eq!(seen[2], "second question");
    assert_eq!(seen.last().map(String::as_str), Some("second answer"));
    // The tool call and its result are in the kept turn; the first turn is hidden.
    assert!(!seen.iter().any(|text| text.contains("first")));
    // The model's requests shrank: three messages, not five, when the turn began.
    let requests = model.requests();
    assert_eq!(requests[1].messages, 3);

    // The log has the cut, and a harness rebuilt from it sees the same conversation.
    let cut = events.matching(&Pattern::new(COMPACTION));
    assert_eq!(cut.len(), 1);
    let replayed = harness_from(&events);
    assert_eq!(seen_by_model(&replayed), seen);
    // Nothing is lost: the whole conversation is still there for a person.
    let all = replayed.journal(&SessionId::new(MAIN)).unwrap().session();
    assert_eq!(all.conversation()[0].text_content(), "first question");
}

fn harness_from(events: &EventStream) -> Harness {
    let model = Arc::new(ScriptedProvider::new([]));
    harness(&model, Arc::new(EventStream::replay(events.events())))
}

#[tokio::test]
async fn without_a_compaction_service_nothing_is_condensed() {
    let model = Arc::new(ScriptedProvider::new([say("a"), say("b"), say("c")]));
    let harness = harness(&model, Arc::new(EventStream::new()));
    for prompt in ["one", "two", "three"] {
        harness.run(prompt).await.unwrap();
    }
    assert_eq!(seen_by_model(&harness).len(), 6);
    assert_eq!(
        harness.events().matching(&Pattern::new(COMPACTION)).len(),
        0
    );
}

#[tokio::test]
async fn the_compact_plugin_summarises_with_a_model_of_its_own() {
    let model = Arc::new(ScriptedProvider::new([
        say("one"),
        say("two"),
        say("three"),
    ]));
    let harness = harness(&model, Arc::new(EventStream::new()));
    let writer = Arc::new(ScriptedProvider::new([say("We talked about one and two.")]));
    harness.plugin(provider("writer", Arc::<ScriptedProvider>::clone(&writer)).serving(["small"]));
    harness.plugin(
        WasmRuntime::new()
            .unwrap()
            .load(plugin("compact"))
            .unwrap()
            .grant("llm", Scope::none().allow("small"))
            .config(json!({"model": "small", "max_messages": 4}).to_string()),
    );

    harness.run("first").await.unwrap();
    harness.run("second").await.unwrap();
    assert_eq!(
        harness.events().matching(&Pattern::new(COMPACTION)).len(),
        0
    );
    harness.run("third").await.unwrap();

    let seen = seen_by_model(&harness);
    assert!(seen[0].contains("We talked about one and two."), "{seen:?}");
    assert_eq!(&seen[2..], ["third", "three"]);
    let asked = writer.requests();
    assert_eq!(asked.len(), 1);
    assert!(asked[0].system.contains("condense"));
    let cut = harness.events().matching(&Pattern::new(COMPACTION));
    let cut: Value = serde_json::from_str(&cut[0].payload).unwrap();
    assert_eq!(cut["by"], COMPACTION);
}
