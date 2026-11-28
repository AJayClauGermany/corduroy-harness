//! Shaping a request: steps that change what the model is sent and leave the record alone.
//! A prototype of one form (G19 in `docs/open-questions.md`; see `docs/prototypes.md`).
//! No network or LLM.

use std::sync::Arc;

use corduroy_harness::core::{Message, ModelRequest, Part, Pattern, plugin_fn};
use corduroy_harness::scripted::{ScriptedProvider, say};
use corduroy_harness::{Harness, MAIN, SHAPERS, Session, Shape, Shapers, Status};
use serde_json::Value;

/// Sends the last `0` messages only.
struct Latest(usize);

impl Shape for Latest {
    fn shape(&self, _: &Session, mut request: ModelRequest) -> ModelRequest {
        let from = request.messages.len().saturating_sub(self.0);
        request.messages = request.messages.split_off(from);
        request
    }
}

/// The text of each message, oldest first.
fn texts(messages: &[Message]) -> Vec<String> {
    messages.iter().map(Message::text_content).collect()
}

/// The payload of every `model/request` in the log of `harness`.
fn requests(harness: &Harness) -> Vec<Value> {
    harness
        .events()
        .matching(&Pattern::new("model/request"))
        .iter()
        .map(|event| serde_json::from_str(&event.payload).unwrap())
        .collect()
}

#[tokio::test]
async fn a_shaper_changes_what_is_sent_and_the_record_stays_whole() {
    let model = Arc::new(ScriptedProvider::new([say("one"), say("two")]));
    let harness = Harness::new(Arc::<ScriptedProvider>::clone(&model)).unwrap();
    harness
        .shapers()
        .insert("latest", Arc::new(Latest(1)))
        .unwrap();

    harness.run("first").await.unwrap();
    let outcome = harness.run("second").await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "two".into()
        }
    );
    // The second request carried the last message only.
    let sent = model.requests();
    assert_eq!(texts(&sent[0].conversation), ["first"]);
    assert_eq!(texts(&sent[1].conversation), ["second"]);
    // The conversation in the log is as it would be without the shaper.
    let session = harness.journal(&MAIN.into()).unwrap().session();
    assert_eq!(
        texts(session.conversation()),
        ["first", "one", "second", "two"]
    );
    assert_eq!(session.for_model().len(), 4);
    // A request the shaper left as it was says nothing; one it changed names it, and what
    // the messages sent came to.
    let recorded = requests(&harness);
    assert_eq!(recorded[0].get("shaped"), None);
    assert_eq!(recorded[0].get("messages_hash"), None);
    assert_eq!(recorded[1]["shaped"], serde_json::json!(["latest"]));
    assert_eq!(recorded[1]["messages"], 1);
    assert_eq!(recorded[1]["messages_hash"].as_str().unwrap().len(), 16);
}

#[tokio::test]
async fn shapers_run_in_name_order_and_none_changes_the_model() {
    let model = Arc::new(ScriptedProvider::new([say("done")]));
    let harness = Harness::new(Arc::<ScriptedProvider>::clone(&model))
        .unwrap()
        .with_model("the-sessions-own");
    let remind = |text: &'static str| {
        move |_: &Session, mut request: ModelRequest| {
            request.system.push_str(text);
            request.model = "another".to_owned();
            request
        }
    };
    // Registered out of order, and one that passes.
    harness
        .shapers()
        .insert("b", Arc::new(remind(" b")))
        .unwrap();
    harness
        .shapers()
        .insert("a", Arc::new(remind(" a")))
        .unwrap();
    let pass = |_: &Session, request: ModelRequest| request;
    harness.shapers().insert("c", Arc::new(pass)).unwrap();

    harness.run("go").await.unwrap();

    let sent = &model.requests()[0];
    assert_eq!(sent.system, " a b");
    assert_eq!(sent.model, "the-sessions-own");
    assert_eq!(
        requests(&harness)[0]["shaped"],
        serde_json::json!(["a", "b"])
    );
}

#[tokio::test]
async fn a_plugin_contributes_a_shaper_and_takes_it_away_when_unloaded() {
    let model = Arc::new(ScriptedProvider::new([say("one"), say("two")]));
    let harness = Harness::new(Arc::<ScriptedProvider>::clone(&model)).unwrap();
    let id = harness.plugin(plugin_fn("trimmer", &[SHAPERS], |ctx| {
        let shapers = ctx.service::<Shapers>(SHAPERS)?;
        shapers.register(ctx, "latest", Arc::new(Latest(1)))?;
        Ok(())
    }));

    harness.run("first").await.unwrap();
    harness.with_app(|app| app.dispose(id)).unwrap();
    harness.run("second").await.unwrap();

    // With the plugin gone, the whole conversation is sent again: nothing was lost.
    let sent = model.requests();
    assert_eq!(texts(&sent[1].conversation), ["first", "one", "second"]);
    assert!(sent[1].conversation.iter().all(|message| {
        message
            .parts
            .iter()
            .all(|part| matches!(part, Part::Text(_)))
    }));
    assert_eq!(requests(&harness)[1].get("shaped"), None);
}
