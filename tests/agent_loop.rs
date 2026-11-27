//! The loop through the public API, with native plugins only. No WebAssembly, network or LLM.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use corduroy_harness::core::{
    Context, EventStream, Message, Pattern, PluginError, SessionId, TOOLS, ToolError, ToolResult,
    ToolSpec, Tools, plugin_fn, tool_fn,
};
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::{
    Effect, Harness, HarnessError, MAIN, Next, OPERATIONS, Operation, Operations, Session,
    SessionInfo, Status, Stepped,
};
use serde_json::json;

/// A native plugin contributing `add`, which sums the numbers in `{"terms": [...]}`.
fn calculator(ctx: &mut Context<'_>) -> Result<(), PluginError> {
    let spec = ToolSpec::new(
        "add",
        "Sum a list of integers.",
        r#"{"type":"object","properties":{"terms":{"type":"array"}}}"#,
    )
    .replay_safe(true)
    .read_only(true);
    let add = tool_fn(spec, |input| {
        let input: serde_json::Value = serde_json::from_str(input)
            .map_err(|error| ToolError::InvalidInput(error.to_string()))?;
        let terms = input
            .get("terms")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| ToolError::InvalidInput("`terms` must be a list".into()))?;
        let total: i64 = terms.iter().filter_map(serde_json::Value::as_i64).sum();
        Ok(total.to_string())
    });
    ctx.service::<Tools>(TOOLS)?
        .register(ctx, "add", Arc::new(add))?;
    Ok(())
}

fn harness(
    replies: impl IntoIterator<Item = Message>,
) -> Result<(Harness, Arc<ScriptedProvider>), HarnessError> {
    let model = Arc::new(ScriptedProvider::new(replies));
    let provider: Arc<ScriptedProvider> = Arc::clone(&model);
    let harness = Harness::new(provider)?;
    harness.plugin(plugin_fn("calculator", &[TOOLS], calculator));
    Ok((harness, model))
}

fn tool_results(harness: &Harness) -> Vec<String> {
    harness
        .events()
        .matching(&Pattern::new("tool/result"))
        .into_iter()
        .map(|event| event.payload)
        .collect()
}

#[tokio::test]
async fn the_model_calls_a_tool_and_answers() {
    let (harness, model) = harness([
        call("c1", "add", &json!({"terms": [2, 3, 4]})),
        say("the total is 9"),
    ])
    .unwrap();

    let outcome = harness.run("add 2, 3 and 4").await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "the total is 9".into()
        }
    );
    assert_eq!(outcome.status.to_string(), "finished: the total is 9");
    assert_eq!(
        tool_results(&harness),
        [r#"{"id":"c1","tool":"add","ok":true,"output":"9","bytes":1,"tokens":1}"#]
    );
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].tools, ["add"]);
    assert_eq!((requests[0].messages, requests[1].messages), (1, 3));
}

#[tokio::test]
async fn every_event_records_its_source_and_cause() {
    let (harness, _model) = harness([call("c1", "add", &json!({"terms": [1]})), say("1")]).unwrap();

    harness.run("add").await.unwrap();

    let trace: Vec<String> = harness
        .events()
        .events()
        .into_iter()
        .filter(|event| event.topic != "usage")
        .map(|event| {
            let cause = event
                .cause
                .map_or_else(String::new, |id| format!(" <- {id}"));
            let session = event
                .session
                .map_or_else(String::new, |session| format!(" in {session}"));
            format!(
                "{} {} {}{session}{cause}",
                event.id, event.source, event.topic
            )
        })
        .collect();
    // Events of the run name its session; the plugin tree's own events name none, and nor
    // does what the model was shown, which any session's request may name.
    assert_eq!(
        trace,
        [
            "#1 host session/start in main",
            "#2 host plugin/activated",
            "#3 user message in main",
            "#4 host model/shown",
            "#5 host model/request in main",
            "#6 agent message in main",
            "#7 agent tool/call in main <- #6",
            "#9 host tool/start in main <- #7",
            "#10 environment tool/result in main <- #7",
            "#11 environment message in main",
            "#12 host model/request in main",
            "#13 agent message in main",
            "#15 host turn/end in main",
        ]
    );
}

#[tokio::test]
async fn tool_failures_go_back_to_the_model_as_data() {
    let (harness, _model) = harness([
        call("c1", "calculator", &json!({})),
        call("c2", "add", &json!({"terms": "2 3"})),
        call("c3", "add", &json!({"terms": [2, 3]})),
        say("5"),
    ])
    .unwrap();

    let outcome = harness.run("add 2 and 3").await.unwrap();

    assert_eq!(outcome.status, Status::Finished { answer: "5".into() });
    assert_eq!(
        tool_results(&harness),
        [
            r#"{"id":"c1","tool":"calculator","ok":false,"output":"unknown tool `calculator`","bytes":25,"tokens":9}"#,
            r#"{"id":"c2","tool":"add","ok":false,"output":"invalid input: `terms` must be a list","bytes":37,"tokens":13}"#,
            r#"{"id":"c3","tool":"add","ok":true,"output":"5","bytes":1,"tokens":1}"#,
        ]
    );
}

#[tokio::test]
async fn a_model_that_never_answers_runs_out_of_turns() {
    let again = |id: &str| call(id, "add", &json!({"terms": [1]}));
    let (harness, model) = harness([
        again("c1"),
        again("c2"),
        again("c3"),
        again("c4"),
        again("c5"),
    ])
    .unwrap();
    let harness = harness.with_max_turns(3);

    let outcome = harness.run("spin").await.unwrap();

    assert_eq!(outcome.status, Status::TurnLimitReached);
    assert_eq!(model.requests().len(), 3);
}

#[tokio::test]
async fn a_failing_model_ends_the_run_with_the_error_recorded() {
    let (harness, _model) = harness([]).unwrap();

    let outcome = harness.run("anyone there?").await.unwrap();

    let Status::Failed { reason } = outcome.status else {
        panic!("expected a failure, got {:?}", outcome.status);
    };
    assert!(reason.contains("the script ran out"), "{reason}");
    let ends = harness.events().matching(&Pattern::new("turn/end"));
    assert_eq!(
        ends[0].payload,
        r#"{"status":"failed","reason":"the script ran out","kind":"other"}"#
    );
}

#[tokio::test]
async fn unloading_a_plugin_takes_its_tools_away_from_the_next_request() {
    let (harness, model) = harness([say("first"), say("second")]).unwrap();
    let calculator = harness.app().fibers()[0].id;

    harness.run("one").await.unwrap();
    harness.app().dispose(calculator).unwrap();
    harness.run("two").await.unwrap();

    let offered: Vec<Vec<String>> = model.requests().into_iter().map(|r| r.tools).collect();
    assert_eq!(offered, [vec!["add".to_owned()], vec![]]);
}

/// An operation that refuses every call to one tool before it can run.
struct Forbid(&'static str);

#[async_trait]
impl Operation for Forbid {
    fn name(&self) -> &'static str {
        "forbid"
    }

    async fn run(&self, session: &Session) -> Result<Next> {
        let refusals = session
            .pending_calls()
            .into_iter()
            .filter(|call| call.name == self.0)
            .fold(Message::user(), |refusals, call| {
                refusals.result(ToolResult::failed(call.id, "not allowed here"))
            });
        if refusals.is_empty() {
            return Ok(Next::Pass);
        }
        Ok(Next::Continue(vec![Effect::from(refusals)]))
    }
}

#[tokio::test]
async fn a_custom_operation_can_step_in_before_tools_run() {
    let (harness, _model) =
        harness([call("c1", "add", &json!({"terms": [1, 1]})), say("ok")]).unwrap();
    let harness = harness.with_operation(Arc::new(Forbid("add")));

    harness.run("add").await.unwrap();

    assert_eq!(
        tool_results(&harness),
        [
            r#"{"id":"c1","tool":"add","ok":false,"output":"not allowed here","bytes":16,"tokens":6}"#
        ]
    );
}

#[tokio::test]
async fn a_native_plugin_contributes_a_step_that_leaves_with_it() {
    let script = [
        call("c1", "add", &json!({"terms": [1, 1]})),
        say("refused"),
        call("c2", "add", &json!({"terms": [2, 2]})),
        say("four"),
    ];
    let (harness, _model) = harness(script).unwrap();
    let policy = harness.plugin(plugin_fn("policy", &[OPERATIONS], |ctx| {
        let operations = ctx.service::<Operations>(OPERATIONS)?;
        operations.register(ctx, "policy/forbid", Arc::new(Forbid("add")))?;
        Ok(())
    }));
    assert_eq!(harness.operations().names(), ["policy/forbid"]);

    harness.run("add").await.unwrap();
    harness.app().dispose(policy).unwrap();
    harness.run("add again").await.unwrap();

    assert_eq!(harness.operations().names(), Vec::<String>::new());
    assert_eq!(
        tool_results(&harness),
        [
            r#"{"id":"c1","tool":"add","ok":false,"output":"not allowed here","bytes":16,"tokens":6}"#,
            r#"{"id":"c2","tool":"add","ok":true,"output":"4","bytes":1,"tokens":1}"#
        ]
    );
}

#[tokio::test]
async fn the_loop_can_be_driven_one_step_at_a_time() {
    let (harness, _model) =
        harness([call("c1", "add", &json!({"terms": [20, 22]})), say("42")]).unwrap();
    let main = SessionId::new(MAIN);
    let journal = harness.journal(&main).unwrap();
    journal
        .record(&Message::user().text("what is 20 + 22?"))
        .unwrap();

    let mut applied_steps = Vec::new();
    while let Stepped::Applied(step) = harness.step(&main).await.unwrap() {
        applied_steps.push(step);
    }

    assert_eq!(applied_steps, ["model", "tools", "model"]);
    assert_eq!(journal.session().conversation().len(), 4);
    assert!(!journal.unfinished());
}

#[tokio::test]
async fn a_run_can_be_resumed_from_its_event_log() {
    let (first, _model) =
        harness([call("c1", "add", &json!({"terms": [1, 2]})), say("3")]).unwrap();
    first.run("add 1 and 2").await.unwrap();
    let log = first.events().events();

    let stream = Arc::new(EventStream::load(log));
    let model = Arc::new(ScriptedProvider::new([say("still 3")]));
    let provider: Arc<ScriptedProvider> = Arc::clone(&model);
    let resumed = Harness::resume(provider, stream).unwrap();
    let outcome = resumed.run("and again?").await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "still 3".into()
        }
    );
    // The earlier conversation came back from the log: four messages, plus the new prompt.
    assert_eq!(model.requests()[0].messages, 5);
}

#[tokio::test]
async fn sessions_keep_separate_conversations_on_one_stream() {
    let (harness, model) = harness([say("main one"), say("child one"), say("main two")]).unwrap();
    let main = SessionId::new(MAIN);

    harness.run("first").await.unwrap();
    let child = harness.start_session("child", Some(&main)).unwrap();
    let outcome = harness.run_in(&child, "hello").await.unwrap();
    harness.run("second").await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "child one".into()
        }
    );
    // The child started empty and the main session never saw the child's messages.
    let messages: Vec<usize> = model.requests().iter().map(|r| r.messages).collect();
    assert_eq!(messages, [1, 1, 3]);
    assert_eq!(
        harness.session_list(),
        [
            SessionInfo {
                id: child.clone(),
                parent: Some(main.clone()),
                task: None,
            },
            SessionInfo {
                id: main,
                parent: None,
                task: None,
            },
        ]
    );
    assert!(
        harness
            .events()
            .session(&child)
            .iter()
            .all(|event| event.session.as_ref() == Some(&child))
    );
}

#[tokio::test]
async fn sessions_come_back_from_the_log_with_their_parents() {
    let (first, _model) = harness([say("from the child")]).unwrap();
    let main = SessionId::new(MAIN);
    let child = first.start_session("child", Some(&main)).unwrap();
    first.run_in(&child, "hello").await.unwrap();

    let stream = Arc::new(EventStream::load(first.events().events()));
    let model = Arc::new(ScriptedProvider::new([say("still here")]));
    let provider: Arc<ScriptedProvider> = Arc::clone(&model);
    let resumed = Harness::resume(provider, stream).unwrap();
    resumed.run_in(&child, "again?").await.unwrap();

    assert_eq!(resumed.session_list(), first.session_list());
    // The child's two earlier messages, and the new prompt. The main session was not
    // started a second time.
    assert_eq!(model.requests()[0].messages, 3);
    let starts = resumed.events().matching(&Pattern::new("session/start"));
    assert_eq!(starts.len(), 2);
}

#[tokio::test]
async fn a_session_must_be_new_and_its_parent_must_exist() {
    let (harness, _model) = harness([]).unwrap();
    let nobody = SessionId::new("nobody");

    let duplicate = harness.start_session(MAIN, None).unwrap_err();
    let orphan = harness.start_session("child", Some(&nobody)).unwrap_err();
    let missing = harness.run_in(&nobody, "hello").await.unwrap_err();

    assert_eq!(duplicate.to_string(), "session `main` already exists");
    assert_eq!(orphan.to_string(), "there is no session `nobody`");
    assert!(matches!(missing, HarnessError::UnknownSession(_)));
    assert_eq!(harness.session_list().len(), 1);
}
