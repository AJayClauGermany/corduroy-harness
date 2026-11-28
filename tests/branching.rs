//! A session that branches from an earlier point of another: it begins with the
//! conversation the other held there, and goes its own way. A prototype (C14 in
//! `docs/open-questions.md`; see `docs/prototypes.md`). No network or LLM.

use std::sync::Arc;

use corduroy_harness::core::{
    EventId, EventStream, Message, SessionId, TOOLS, ToolSpec, Tools, plugin_fn, tool_fn,
};
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::{Harness, HarnessError, MAIN, Status, removal};
use serde_json::json;

/// The text of each message of `session`, oldest first.
fn said(harness: &Harness, session: &str) -> Result<Vec<String>, HarnessError> {
    let journal = harness.journal(&SessionId::new(session))?;
    Ok(journal
        .session()
        .conversation()
        .iter()
        .map(Message::text_content)
        .collect())
}

/// The id of the `message` event of `session` whose text is `text`.
fn message(harness: &Harness, session: &str, text: &str) -> Option<EventId> {
    harness
        .events()
        .session(&SessionId::new(session))
        .into_iter()
        .find(|event| event.topic == "message" && event.payload.contains(text))
        .map(|event| event.id)
}

#[tokio::test]
async fn a_branch_begins_where_the_other_session_stood_and_goes_its_own_way() {
    let model = Arc::new(ScriptedProvider::new([
        say("one"),
        say("two"),
        say("three"),
        say("four"),
    ]));
    let events = Arc::new(EventStream::new());
    let harness =
        Harness::resume(Arc::<ScriptedProvider>::clone(&model), Arc::clone(&events)).unwrap();
    let main = SessionId::new(MAIN);
    harness.run("first").await.unwrap();
    harness.run("second").await.unwrap();
    let after_one = message(&harness, MAIN, "one").unwrap();
    let before = events.events().len();

    // Branch after the first answer: the second exchange is not the branch's.
    let alt = harness.branch_session("alt", &main, after_one).unwrap();
    // One event was stored for it: its start, which names where it came from.
    let stored = events.events();
    assert_eq!(stored.len(), before + 1);
    let start = stored.last().unwrap();
    assert_eq!(start.topic, "session/start");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&start.payload).unwrap(),
        json!({"parent": "main", "from": after_one.get()})
    );
    assert_eq!(said(&harness, "alt").unwrap(), ["first", "one"]);

    let outcome = harness.run_in(&alt, "another way").await.unwrap();
    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "three".into()
        }
    );
    // The model was sent the branch's conversation, and the other session is as it was.
    let sent: Vec<String> = model.requests()[2]
        .conversation
        .iter()
        .map(Message::text_content)
        .collect();
    assert_eq!(sent, ["first", "one", "another way"]);
    assert_eq!(
        said(&harness, MAIN).unwrap(),
        ["first", "one", "second", "two"]
    );
    harness.run("fourth").await.unwrap();
    assert_eq!(said(&harness, MAIN).unwrap().len(), 6);
    assert_eq!(
        said(&harness, "alt").unwrap(),
        ["first", "one", "another way", "three"]
    );
}

#[tokio::test]
async fn a_branch_is_read_back_from_the_log_branched_again_and_removed_with_its_origin() {
    let model = Arc::new(ScriptedProvider::new([say("one"), say("three")]));
    let events = Arc::new(EventStream::new());
    let harness = Harness::resume(model, Arc::clone(&events)).unwrap();
    let main = SessionId::new(MAIN);
    harness.run("first").await.unwrap();
    let after_one = message(&harness, MAIN, "one").unwrap();
    let alt = harness.branch_session("alt", &main, after_one).unwrap();
    harness.run_in(&alt, "another way").await.unwrap();

    // Another process reads the branch back from the log as it is, with nothing left to do.
    let again = Harness::resume(
        Arc::new(ScriptedProvider::new([])),
        Arc::new(EventStream::load(events.events())),
    )
    .unwrap();
    let whole = ["first", "one", "another way", "three"];
    assert_eq!(said(&again, "alt").unwrap(), whole);
    assert!(!again.journal(&alt).unwrap().unfinished());
    // A branch of a branch begins with both.
    let after_three = message(&again, "alt", "three").unwrap();
    again.branch_session("alt-2", &alt, after_three).unwrap();
    assert_eq!(said(&again, "alt-2").unwrap(), whole);
    // Removing a session removes what branched from it, whose beginning it holds.
    let removed = removal(again.events(), &main).unwrap();
    assert!(removed.sessions.contains(&alt) && removed.sessions.contains(&"alt-2".into()));
}

#[tokio::test]
async fn a_branch_begins_only_where_the_conversation_is_whole() {
    let model = Arc::new(ScriptedProvider::new([
        call("c1", "noop", &json!({})),
        say("done"),
    ]));
    let harness = Harness::new(model).unwrap();
    harness.plugin(plugin_fn("noop", &[TOOLS], |ctx| {
        let spec = ToolSpec::new("noop", "", r#"{"type":"object"}"#);
        let tool = Arc::new(tool_fn(spec, |_| Ok("nothing")));
        ctx.service::<Tools>(TOOLS)?.register(ctx, "noop", tool)?;
        Ok(())
    }));
    let main = SessionId::new(MAIN);
    harness.run("work").await.unwrap();
    let refused = |at: EventId| {
        harness
            .branch_session("alt", &main, at)
            .unwrap_err()
            .to_string()
    };
    let topic = |topic: &str| {
        let events = harness.events().session(&main);
        events
            .into_iter()
            .find(|event| event.topic == topic)
            .map(|event| event.id)
            .unwrap()
    };

    // After what the person said, the model has not answered; after the model's call, the
    // call has no result.
    let asked = message(&harness, MAIN, "work").unwrap();
    assert!(
        refused(asked).contains("the model has not answered"),
        "{}",
        refused(asked)
    );
    let called = topic("tool/call");
    assert!(
        refused(called).contains("a tool call has no result"),
        "{}",
        refused(called)
    );
    // An event of another session, or of none, is no place to branch this one.
    harness.start_session("other", None).unwrap();
    let elsewhere = harness.events().session(&"other".into())[0].id;
    assert!(refused(elsewhere).contains("is not an event of session `main`"));
    assert!(matches!(
        harness.branch_session("alt", &"nobody".into(), asked),
        Err(HarnessError::UnknownSession(_))
    ));
    // Before anything was said, and after the answer, it can: a new beginning, and a copy.
    harness
        .branch_session("empty", &main, topic("session/start"))
        .unwrap();
    assert_eq!(said(&harness, "empty").unwrap(), Vec::<String>::new());
    harness
        .branch_session("whole", &main, topic("turn/end"))
        .unwrap();
    assert_eq!(said(&harness, "whole").unwrap().last().unwrap(), "done");
    assert!(matches!(
        harness.branch_session("whole", &main, topic("turn/end")),
        Err(HarnessError::DuplicateSession(_))
    ));
}
