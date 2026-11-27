//! What keeps a conversation within what its model holds: a bound on each tool result and on
//! a step's results together, compaction inside a turn, and a session that can be used
//! again after a request was too much. No network or LLM.

use std::sync::{Arc, Mutex};

use corduroy_harness::core::{
    CallError, EventStream, Keep, Message, ModelInfo, ModelReply, ModelRequest, Part, Pattern,
    Service, SessionId, TOOLS, ToolCall, ToolSpec, Tools, Usage, plugin_fn, tool_fn,
};
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::{
    COMPACTION, Harness, MAIN, Provider, ProviderError, ProviderErrorKind, Status,
};
use serde_json::{Value, json};

/// A model that holds `context` tokens of three bytes, counts what it is sent, and refuses a
/// request that is more than it holds.
struct Holding {
    script: ScriptedProvider,
    context: u64,
}

/// The bytes of text, tool calls and tool results in `messages`.
fn bytes(messages: &[Message]) -> usize {
    messages
        .iter()
        .flat_map(|message| &message.parts)
        .map(|part| match part {
            Part::Text(text) => text.len(),
            Part::ToolCall(call) => call.input.len(),
            Part::ToolResult(result) => result.output.to_text().len(),
            _ => 0,
        })
        .sum()
}

#[async_trait::async_trait]
impl Provider for Holding {
    async fn complete(&self, request: &ModelRequest) -> Result<ModelReply, ProviderError> {
        let tokens = u64::try_from(bytes(&request.messages)).unwrap() / 3;
        if tokens > self.context {
            return Err(ProviderError::new("Context length exceeded")
                .of(ProviderErrorKind::ContextOverflow));
        }
        let reply = self.script.complete(request).await?;
        Ok(ModelReply::new(
            reply.message,
            Usage::new(Some(tokens), Some(5)),
        ))
    }

    fn describe(&self, _model: &str) -> ModelInfo {
        ModelInfo::unknown().with_context(self.context)
    }
}

/// A harness on `events` whose model holds `context` tokens and gives `replies`, with a
/// `big` tool that returns as many bytes as it is asked for and says it keeps the start.
fn agent(context: u64, replies: Vec<Message>, events: Arc<EventStream>) -> (Harness, Arc<Holding>) {
    let model = Arc::new(Holding {
        script: ScriptedProvider::new(replies),
        context,
    });
    let harness = Harness::resume(Arc::<Holding>::clone(&model), events).unwrap();
    with_big(&harness);
    (harness, model)
}

/// Give `harness` the `big` tool.
fn with_big(harness: &Harness) {
    harness.plugin(plugin_fn("big", &[TOOLS], |ctx| {
        let spec = ToolSpec::new("big", "", r#"{"type":"object"}"#)
            .replay_safe(true)
            .read_only(true)
            .result(16_384, Keep::Head);
        let tool = tool_fn(spec, |input| {
            let input: Value = serde_json::from_str(input).unwrap_or(Value::Null);
            let size = usize::try_from(input["bytes"].as_u64().unwrap_or(0)).unwrap_or(0);
            let line = format!("{} line\n", input["name"].as_str().unwrap_or("a"));
            Ok(line.repeat(size / line.len()))
        });
        ctx.service::<Tools>(TOOLS)?
            .register(ctx, "big", Arc::new(tool))?;
        Ok(())
    }));
}

/// A compaction service that is due as `due` says of what it is asked, and keeps what it
/// was asked.
fn service(asked: &Arc<Mutex<Vec<Value>>>, due: fn(&Value) -> bool) -> Arc<dyn Service> {
    let asked = Arc::clone(asked);
    Arc::new(move |_caller: &str, method: &str, payload: &str| {
        let payload: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
        let answer = match method {
            "due" => json!({"due": due(&payload)}),
            "summarize" => json!({"summary": "SUMMARY"}),
            other => return Err(CallError::Unknown(other.to_owned())),
        };
        asked
            .lock()
            .unwrap()
            .push(json!({"method": method, "payload": payload}));
        Ok(answer.to_string())
    })
}

fn payloads(events: &EventStream, topic: &str) -> Vec<Value> {
    events
        .matching(&Pattern::new(topic))
        .iter()
        .map(|event| serde_json::from_str(&event.payload).unwrap())
        .collect()
}

#[tokio::test]
async fn eight_results_in_one_step_are_cut_to_what_the_model_has_room_for() {
    // Eight reads of 50 KB each, asked for in one message: 133 000 tokens of results for a
    // model that holds 16 384.
    let mut reads = Message::assistant();
    for read in 0..8 {
        let input = json!({"bytes": 50_000, "name": format!("file{read}")});
        reads = reads.call(ToolCall::new(read.to_string(), "big", input.to_string()));
    }
    let events = Arc::new(EventStream::new());
    let (harness, model) = agent(16_384, vec![reads, say("read them")], Arc::clone(&events));

    let outcome = harness.run("read the eight files").await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "read them".into()
        }
    );
    // The model was shown a quarter of its context at most, shared between the results,
    // each from its start and each saying what was left out.
    let requests = model.script.requests();
    let shown: Vec<String> = requests[1].conversation[2]
        .tool_results()
        .map(|result| result.output.to_text())
        .collect();
    assert_eq!(shown.len(), 8);
    let total: usize = shown.iter().map(String::len).sum();
    assert!(total <= 16_384 / 4 * 3, "{total} bytes shown");
    for (read, text) in shown.iter().enumerate() {
        assert!(text.starts_with(&format!("file{read} line\n")), "{text}");
        assert!(text.contains("[cut: the first "), "{text}");
        assert!(text.contains("of 49995 bytes are shown"), "{text}");
    }
    // The log has what each tool returned, whole.
    let results = payloads(&events, "tool/result");
    assert_eq!(results.len(), 8);
    assert!(
        results
            .iter()
            .all(|result| result["output"].as_str().unwrap().len() == 49_995)
    );
    // And a harness that reads the log back shows the model what this one did.
    let (reloaded, _) = harness_from(&events);
    let main = SessionId::new(MAIN);
    assert_eq!(
        reloaded.journal(&main).unwrap().session().for_model(),
        harness.journal(&main).unwrap().session().for_model()
    );
}

fn harness_from(events: &EventStream) -> (Harness, Arc<Holding>) {
    agent(
        16_384,
        Vec::new(),
        Arc::new(EventStream::load(events.events())),
    )
}

#[tokio::test]
async fn one_result_is_held_to_its_tools_budget_where_the_model_says_nothing() {
    // The tool asks for 16 384 tokens at most, 49 152 bytes, and nothing is known of the
    // model: a result within that is left alone, a longer one is cut to it.
    let model = Arc::new(ScriptedProvider::new([
        call("1", "big", &json!({"bytes": 40_000})),
        call("2", "big", &json!({"bytes": 90_000})),
        say("done"),
    ]));
    let harness = Harness::new(Arc::<ScriptedProvider>::clone(&model)).unwrap();
    with_big(&harness);

    harness.run("two reads").await.unwrap();

    let requests = model.requests();
    let last = &requests[2].conversation;
    let sizes: Vec<usize> = last
        .iter()
        .flat_map(Message::tool_results)
        .map(|result| result.output.to_text().len())
        .collect();
    assert_eq!(sizes[0], 39_998);
    assert!(sizes[1] <= 49_152 + 200 && sizes[1] > 40_000, "{sizes:?}");
}

/// What the compaction service was asked, in order.
fn calls(asked: &Mutex<Vec<Value>>) -> Vec<Value> {
    asked.lock().unwrap().clone()
}

/// A session on a model of 1200 tokens (3600 bytes) whose second prompt made a request the
/// model could not hold, with a compaction service that is due only for a session that
/// overflowed.
async fn overflowed(replies: Vec<Message>) -> (Harness, Arc<EventStream>, Arc<Mutex<Vec<Value>>>) {
    let events = Arc::new(EventStream::new());
    let (harness, _) = agent(1200, replies, Arc::clone(&events));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let due = service(&asked, |payload| payload["overflowed"] == true);
    harness.services().insert(COMPACTION, due).unwrap();
    harness.run(&"a".repeat(3000)).await.unwrap();
    let failed = harness.run(&"b".repeat(3000)).await.unwrap();
    assert!(
        matches!(failed.status, Status::Failed { .. }),
        "{:?}",
        failed.status
    );
    (harness, events, asked)
}

#[tokio::test]
async fn a_session_whose_request_was_too_much_can_be_used_again() {
    let (harness, events, asked) =
        overflowed(vec![say("first"), say("third"), say("fourth")]).await;
    assert_eq!(payloads(&events, "turn/end")[1]["kind"], "context-overflow");
    assert_eq!(payloads(&events, COMPACTION).len(), 0);

    // The next run is told the session overflowed, summarises what came before, and the
    // model is asked something it can hold.
    let outcome = harness.run("and now?").await.unwrap();

    let answer = "third".into();
    assert_eq!(outcome.status, Status::Finished { answer });
    assert_eq!(payloads(&events, COMPACTION).len(), 1);
    let first = calls(&asked);
    let due = first.iter().rev().find(|call| call["method"] == "due");
    let due = &due.unwrap()["payload"];
    assert_eq!(due["overflowed"], true);
    // What the next request would take counts what was added since the last answer.
    assert!(due["input_tokens"].as_u64() >= Some(2000), "{due}");
    assert_eq!(due["context_tokens"], 1200);
    let summarize = first.iter().find(|call| call["method"] == "summarize");
    assert_eq!(summarize.unwrap()["payload"]["context_tokens"], 1200);
    let main = SessionId::new(MAIN);
    let seen = harness.journal(&main).unwrap().session().for_model();
    assert!(seen[0].text_content().contains("SUMMARY"), "{seen:?}");
    // Once summarised and answered, the session is no longer said to have overflowed.
    harness
        .run("and then?")
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    let asked = calls(&asked);
    let last = asked.iter().rev().find(|call| call["method"] == "due");
    assert_eq!(last.unwrap()["payload"]["overflowed"], Value::Null);
}

#[tokio::test]
async fn a_log_that_ends_in_an_overflow_is_read_back_as_one() {
    let (_, events, _) = overflowed(vec![say("first")]).await;
    let (reloaded, _) = agent(
        1200,
        vec![say("again")],
        Arc::new(EventStream::load(events.events())),
    );
    let asked = Arc::new(Mutex::new(Vec::new()));
    let due = service(&asked, |payload| payload["overflowed"] == true);
    reloaded.services().insert(COMPACTION, due).unwrap();

    let outcome = reloaded.run("and now?").await.unwrap();

    let answer = "again".into();
    assert_eq!(outcome.status, Status::Finished { answer });
    assert_eq!(payloads(reloaded.events(), COMPACTION).len(), 1);
}

/// A run of three steps on a model that holds plenty, with a compaction service that is
/// due once three messages would give way.
async fn long_turn() -> (Harness, Arc<Holding>, Arc<EventStream>, Vec<Value>) {
    let events = Arc::new(EventStream::new());
    let replies = vec![
        call("1", "big", &json!({"bytes": 60})),
        call("2", "big", &json!({"bytes": 60})),
        call("3", "big", &json!({"bytes": 60})),
        say("done"),
    ];
    let (harness, model) = agent(16_384, replies, Arc::clone(&events));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let due = service(&asked, |payload| payload["messages"].as_u64() >= Some(3));
    harness.services().insert(COMPACTION, due).unwrap();
    let outcome = harness.run("count the lines").await.unwrap();
    let answer = "done".into();
    assert_eq!(outcome.status, Status::Finished { answer });
    let asked = calls(&asked);
    (harness, model, events, asked)
}

#[tokio::test]
async fn one_long_turn_is_summarised_while_it_runs() {
    let (harness, model, events, asked) = long_turn().await;

    // Once: after the second step, when the question and the first step could give way.
    let cuts = payloads(&events, COMPACTION);
    assert_eq!(cuts.len(), 1);
    let messages = events.matching(&Pattern::new("message"));
    // It keeps from the model's second message on: inside the turn, where a step begins.
    assert_eq!(cuts[0]["keep_from"].as_u64(), Some(messages[3].id.get()));
    // The person's request is kept in their words, since the model no longer sees it.
    let summary = cuts[0]["summary"].as_str().unwrap();
    assert!(summary.starts_with("SUMMARY"), "{summary}");
    assert!(
        summary.ends_with("in their words:\ncount the lines"),
        "{summary}"
    );
    // The third request held the summary, the second step's call and its result.
    let requests = model.script.requests();
    assert_eq!(requests.len(), 4);
    let third = &requests[2].conversation;
    assert_eq!(third.len(), 3, "{third:?}");
    assert!(third[0].text_content().contains("count the lines"));
    let called: Vec<&str> = third[1].tool_calls().map(|call| call.id.as_str()).collect();
    let answered: Vec<&str> = third[2]
        .tool_results()
        .map(|result| result.id.as_str())
        .collect();
    assert_eq!((called, answered), (vec!["2"], vec!["2"]));
    // The summariser was shown the question and the first step, and no more.
    let summarize = asked.iter().find(|call| call["method"] == "summarize");
    let transcript = summarize.unwrap()["payload"]["transcript"]
        .as_str()
        .unwrap();
    assert!(
        transcript.starts_with("user: count the lines\nassistant: called big"),
        "{transcript}"
    );
    assert_eq!(transcript.matches("called big").count(), 1, "{transcript}");
    // Nothing is lost to a person, and the log rebuilds what the model sees.
    let main = SessionId::new(MAIN);
    let session = harness.journal(&main).unwrap().session();
    assert_eq!(session.conversation()[0].text_content(), "count the lines");
    assert_eq!(session.conversation().len(), 8);
    let (reloaded, _) = harness_from(&events);
    assert_eq!(
        reloaded.journal(&main).unwrap().session().for_model(),
        session.for_model()
    );
}
