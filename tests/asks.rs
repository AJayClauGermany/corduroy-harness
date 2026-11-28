//! An ask a gate puts to a person anywhere but a tool call of the model's: at `session/start`
//! and at `plugin/load`. A prototype of the form in which what was asked about fails, the
//! ask waits in the log, and the same request goes through once a person has allowed it
//! (C15 in `docs/open-questions.md`; see `docs/prototypes.md`). Uses the `subagents` and
//! `testbed` plugins from `plugins/`. No network or LLM.

use std::sync::Arc;

use corduroy_harness::core::{
    Draft, EventStream, Gate, Pattern, Scope, SessionId, Source, Verdict,
};
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::wasm::WasmRuntime;
use corduroy_harness::{Answer, Harness, MAIN, Status};
use corduroy_test_support::{plugin, scratch};
use serde_json::{Value, json};

/// A harness on `events` whose model gives `replies`, with `delegate` (the `subagents`
/// add-on) and a gate, `careful`, that asks a person about everything at `point`.
fn harness(
    replies: Vec<corduroy_harness::core::Message>,
    events: Arc<EventStream>,
    point: &'static str,
) -> Harness {
    let harness = Harness::resume(Arc::new(ScriptedProvider::new(replies)), events).unwrap();
    harness.plugin(
        WasmRuntime::new()
            .unwrap()
            .load(plugin("subagents"))
            .unwrap()
            .grant("service", Scope::none().allow("sessions:start")),
    );
    let careful: Arc<dyn Gate> = Arc::new(move |asked: &str, _: &str| {
        (asked == point).then(|| Verdict::Ask("a person decides this".into()))
    });
    harness.gates().insert("careful", careful).unwrap();
    harness
}

fn delegate(id: &str) -> corduroy_harness::core::Message {
    call(id, "delegate", &json!({"task": "count the files"}))
}

/// The text of each tool result the model was given in `session`.
fn results(harness: &Harness, session: &str) -> Vec<String> {
    let journal = harness.journal(&SessionId::new(session)).unwrap();
    journal
        .session()
        .conversation()
        .iter()
        .flat_map(|message| message.tool_results().cloned().collect::<Vec<_>>())
        .map(|result| result.output.to_text())
        .collect()
}

/// Every `gate/*` event about an ask, as `<topic> <source> <verdict or allow>`.
fn trail(harness: &Harness) -> Vec<String> {
    harness
        .events()
        .matching(&Pattern::new("gate/*"))
        .into_iter()
        .filter_map(|event| {
            let payload: Value = serde_json::from_str(&event.payload).ok()?;
            payload.get("ask")?;
            let said = payload.get("verdict").or_else(|| payload.get("allow"))?;
            Some(format!("{} {} {said}", event.topic, event.source))
        })
        .collect()
}

#[tokio::test]
async fn what_a_gate_asks_at_a_sessions_start_waits_for_a_person_and_goes_through_when_asked_again()
{
    let replies = vec![
        delegate("a"),
        say("I asked, and will wait."),
        delegate("b"),
        say("three files"),
        say("There are three."),
        delegate("c"),
        say("Asked again."),
    ];
    let harness = harness(replies, Arc::new(EventStream::new()), "session/start");
    harness.answers_asks();

    // Asked for, the session is not started: a person is asked, and the model is told.
    harness.run("how many files?").await.unwrap();
    let told = results(&harness, MAIN);
    assert!(
        told[0].contains("gate `careful` asked a person"),
        "{told:?}"
    );
    assert_eq!(harness.session_list().len(), 1);
    let open = harness.open_asks().unwrap();
    let [ask] = open.as_slice() else {
        panic!("one ask should wait: {open:?}");
    };
    assert!(told[0].contains(&format!("(ask {})", ask.id)), "{told:?}");
    assert_eq!(
        (ask.point.as_str(), ask.gate.as_str(), ask.reason.as_str()),
        ("session/start", "careful", "a person decides this")
    );
    assert_eq!(ask.session, Some(SessionId::new(MAIN)));
    let request: Value = serde_json::from_str(&ask.request).unwrap();
    assert_eq!(request["task"], "count the files");

    // The person allows it. Nothing happens by that; the ask is over.
    assert!(harness.answer_ask(&ask.id, &Answer::Allow).unwrap());
    assert!(!harness.answer_ask(&ask.id, &Answer::Allow).unwrap());
    assert_eq!(harness.open_asks().unwrap(), []);
    assert_eq!(harness.session_list().len(), 1);

    // Asked for again, the same session starts, and its answer comes back.
    let outcome = harness.run("try again").await.unwrap();
    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "There are three.".into()
        }
    );
    assert_eq!(results(&harness, MAIN)[1], "three files");
    assert_eq!(
        trail(&harness),
        [
            r#"gate/decision host "ask""#,
            "gate/answer user true",
            r#"gate/decision host "allow""#,
        ]
    );

    // The answer was for one asking: the next is a question again.
    harness.run("and once more").await.unwrap();
    assert_eq!(harness.open_asks().unwrap().len(), 1);
}

#[tokio::test]
async fn an_ask_is_answered_by_a_person_only_and_from_any_process() {
    let events = Arc::new(EventStream::new());
    let first = harness(
        vec![delegate("a"), say("waiting")],
        Arc::clone(&events),
        "session/start",
    );
    first.answers_asks();
    first.run("how many files?").await.unwrap();
    let id = first.open_asks().unwrap()[0].id.clone();

    // An add-on that raises an answer of its own has answered nothing.
    let forged = json!({"point": "session/start", "ask": id, "allow": true, "reason": ""});
    let forged =
        Draft::new(Source::Plugin("subagents".into()), "gate/answer").payload(forged.to_string());
    events.emit(forged).unwrap();
    assert_eq!(first.open_asks().unwrap().len(), 1);

    // Another process finds the ask in the log, and a refusal there reaches the model.
    let replies = vec![delegate("b"), say("refused")];
    let again = harness(
        replies,
        Arc::new(EventStream::load(events.events())),
        "session/start",
    );
    again.answers_asks();
    assert_eq!(again.open_asks().unwrap()[0].id, id);
    let no = Answer::Deny("not on this machine".into());
    assert!(again.answer_ask(&id, &no).unwrap());
    again.run("try again").await.unwrap();

    let told = results(&again, MAIN);
    assert!(
        told[1].ends_with("a person declined this: not on this machine"),
        "{told:?}"
    );
    assert_eq!(again.session_list().len(), 1);
    assert_eq!(again.open_asks().unwrap(), []);
}

#[tokio::test]
async fn a_host_that_takes_no_answers_leaves_an_ask_a_denial() {
    let harness = harness(
        vec![delegate("a"), say("refused")],
        Arc::new(EventStream::new()),
        "session/start",
    );

    harness.run("how many files?").await.unwrap();

    let told = results(&harness, MAIN);
    assert!(told[0].contains("cannot wait for yet"), "{told:?}");
    assert_eq!(harness.open_asks().unwrap(), []);
    assert_eq!(trail(&harness), Vec::<String>::new());
}

#[tokio::test]
async fn a_load_a_gate_asked_about_goes_through_once_a_person_allowed_it() {
    let work = scratch("asks-load");
    let child = plugin("pi-tools");
    let load = |id: &str| {
        let grants = json!([
            {"capability": "fs-read", "allow": [], "deny": []},
            {"capability": "fs-write", "allow": [], "deny": []},
            {"capability": "kernel", "allow": [], "deny": []},
        ]);
        call(
            id,
            "l.load",
            &json!({"path": child, "name": "child-tools", "grants": grants}),
        )
    };
    let replies = vec![load("a"), say("asked"), load("b"), say("loaded")];
    let harness = harness(replies, Arc::new(EventStream::new()), "plugin/load");
    harness.answers_asks();
    let mut loader = WasmRuntime::new()
        .unwrap()
        .with_root(&work)
        .load(plugin("testbed"))
        .unwrap()
        .named("loader")
        .config(json!({"prefix": "l."}).to_string());
    for capability in loader.requires().to_vec() {
        loader = loader.grant(capability, Scope::none());
    }
    harness.plugin(
        loader
            .grant("plugins", Scope::none().allow("child-*"))
            .grant("fs-read", Scope::any()),
    );

    harness.run("load the tools").await.unwrap();
    let told = results(&harness, MAIN);
    assert!(
        told[0].contains("asked a person whether this may be loaded"),
        "{told:?}"
    );
    assert!(harness.tools().get("read").is_none());
    let open = harness.open_asks().unwrap();
    assert_eq!(open[0].point, "plugin/load");
    let request: Value = serde_json::from_str(&open[0].request).unwrap();
    assert_eq!(
        (&request["loader"], &request["name"]),
        (&json!("plugin:loader"), &json!("child-tools"))
    );

    assert!(harness.answer_ask(&open[0].id, &Answer::Allow).unwrap());
    harness.run("try again").await.unwrap();

    assert!(
        harness.tools().get("read").is_some(),
        "{:?}",
        results(&harness, MAIN)
    );
    assert_eq!(harness.open_asks().unwrap(), []);
}
