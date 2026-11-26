//! Runs that outlive the process: the log on disk, and picking a run up from it.
//! No WebAssembly, network or LLM.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;

use corduroy_harness::core::{
    Content, Context, Event, EventId, EventStream, Log, LogError, Message, Pattern, Plugin,
    PluginError, Query, SessionId, TOOLS, ToolCall, ToolSpec, Tools, plugin_fn, tool_fn,
};
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::{Harness, HarnessError, MAIN, Next, Operation, Session, Status};
use corduroy_jsonl as jsonl;
use corduroy_test_support::scratch;
use serde_json::{Value, json};

/// A native plugin contributing `echo`, which returns its input.
fn echo(ctx: &mut Context<'_>) -> Result<(), PluginError> {
    let spec = ToolSpec::new("echo", "Return the input.", r#"{"type":"object"}"#)
        .replay_safe(true)
        .read_only(true);
    ctx.service::<Tools>(TOOLS)?.register(
        ctx,
        "echo",
        Arc::new(tool_fn(spec, |input| Ok(input.to_owned()))),
    )?;
    Ok(())
}

/// A harness over `events` whose model gives `replies`, with the `echo` plugin loaded.
fn harness(
    events: EventStream,
    replies: impl IntoIterator<Item = Message>,
) -> Result<(Harness, Arc<ScriptedProvider>), HarnessError> {
    let model = Arc::new(ScriptedProvider::new(replies));
    let provider: Arc<ScriptedProvider> = Arc::clone(&model);
    let harness = Harness::resume(provider, Arc::new(events))?;
    harness.plugin(plugin_fn("echo", &[TOOLS], echo));
    Ok((harness, model))
}

fn lines(path: &Path) -> usize {
    std::fs::read_to_string(path).unwrap().lines().count()
}

#[tokio::test]
async fn a_run_stored_on_disk_carries_on_in_a_new_process() {
    let path = scratch("durable-carries-on").join("run.jsonl");
    let (first, _model) = harness(
        jsonl::open(&path).unwrap(),
        [
            call("c1", "echo", &json!({"text": "hi"})),
            say("it said hi"),
        ],
    )
    .unwrap();
    first.run("echo hi").await.unwrap();
    let written = first.events().events();
    // Dropping the harness is all a crash would leave: the file.
    drop(first);

    let (second, model) = harness(jsonl::open(&path).unwrap(), [say("hi, as before")]).unwrap();
    let before = second.events().events();
    let outcome = second.run("what did it say?").await.unwrap();

    // The second harness logged its own plugin being activated on top of the first run.
    assert_eq!(before.get(..written.len()), Some(written.as_slice()));
    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "hi, as before".into()
        }
    );
    // Prompt, call, result and answer came back from the file, then the new prompt.
    assert_eq!(model.requests()[0].messages, 5);
}

#[tokio::test]
async fn each_step_of_the_loop_is_one_line_of_the_log() {
    let path = scratch("durable-steps").join("run.jsonl");
    let (harness, _model) = harness(
        jsonl::open(&path).unwrap(),
        [call("c1", "echo", &json!({})), say("done")],
    )
    .unwrap();
    let setup = lines(&path);

    harness.run("go").await.unwrap();

    // The prompt; the request to the model; the model's call (message, tool/call and usage
    // together); the tool starting; its result; the message handing the result to the
    // model; the second request; the answer; and the end of the turn.
    assert_eq!(lines(&path) - setup, 9);
}

/// A log that stores nothing and refuses every commit after the first `healthy` ones.
struct Failing {
    healthy: usize,
    commits: Arc<AtomicUsize>,
}

impl Log for Failing {
    fn commit(&mut self, _batch: &[Event]) -> Result<(), LogError> {
        if self.commits.fetch_add(1, Ordering::SeqCst) < self.healthy {
            Ok(())
        } else {
            Err(LogError::new("the disk is full"))
        }
    }
}

/// A store that hands the stream only the events that belong to no session, and answers
/// for the rest when asked, as one that reads old sessions on request does.
struct Shelf(Vec<Event>);

impl Shelf {
    /// A stream over this store, holding what it hands over.
    fn open(self) -> EventStream {
        let held: Vec<Event> = self
            .0
            .iter()
            .filter(|event| event.session.is_none())
            .cloned()
            .collect();
        let last = self.0.last().map(|event| (event.id, event.time));
        let (last, time) = last.unwrap_or((EventId::new(0), 0));
        EventStream::load(held)
            .continuing(last, time)
            .with_log(self)
    }
}

impl Log for Shelf {
    fn commit(&mut self, batch: &[Event]) -> Result<(), LogError> {
        self.0.extend_from_slice(batch);
        Ok(())
    }

    fn read(
        &mut self,
        query: &Query,
        keep: &dyn Fn(&Event) -> bool,
    ) -> Result<Vec<Event>, LogError> {
        Ok(self
            .0
            .iter()
            .filter(|event| event.id > query.after && query.selects(event) && keep(event))
            .take(query.limit)
            .cloned()
            .collect())
    }
}

#[tokio::test]
async fn sessions_the_stream_was_not_handed_are_read_from_the_store() {
    let (first, _model) = harness(
        EventStream::new(),
        [
            call("c1", "echo", &json!({"text": "hi"})),
            say("it said hi"),
        ],
    )
    .unwrap();
    first.run("echo hi").await.unwrap();
    let stored = first.events().events();
    drop(first);

    let (second, model) = harness(Shelf(stored).open(), [say("still here")]).unwrap();
    // Nothing of the session is in memory, yet it is known and its conversation is whole.
    assert_eq!(second.events().session(&SessionId::new(MAIN)).len(), 0);
    assert!(
        second
            .session_list()
            .iter()
            .any(|info| info.id.as_str() == MAIN)
    );
    let outcome = second.run("and again?").await.unwrap();

    assert!(matches!(outcome.status, Status::Finished { .. }));
    // The prompt, the call, its result and the answer, then the new prompt.
    assert_eq!(model.requests()[0].messages, 5);
    let ids: Vec<u64> = second
        .events()
        .events()
        .iter()
        .map(|e| e.id.get())
        .collect();
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{ids:?}");
}

#[tokio::test]
async fn a_step_the_log_refuses_ends_the_run_before_anyone_sees_it() {
    let commits = Arc::new(AtomicUsize::new(0));
    // Room for the session's start, the plugin's activation, the prompt, the request to
    // the model and the model's call, but not the result.
    let log = Failing {
        healthy: 5,
        commits: Arc::clone(&commits),
    };
    let (harness, model) = harness(
        EventStream::new().with_log(log),
        [call("c1", "echo", &json!({})), say("never reached")],
    )
    .unwrap();

    let error = harness.run("go").await.unwrap_err();

    assert!(matches!(error, HarnessError::Log(_)), "{error:?}");
    assert_eq!(error.to_string(), "event log: the disk is full");
    // The result that was not stored is in neither the stream nor the conversation, and
    // the model was not asked again.
    assert_eq!(harness.events().len(), 7);
    let journal = harness.journal(&MAIN.into()).unwrap();
    assert_eq!(journal.session().conversation().len(), 2);
    assert_eq!(model.requests().len(), 1);
}

/// A plugin contributing `launch`, which is not safe to run twice and counts its calls, and
/// `peek`, which is and counts its own.
fn rocketry(launches: &Arc<AtomicUsize>, peeks: &Arc<AtomicUsize>) -> impl Plugin + 'static {
    let (launches, peeks) = (Arc::clone(launches), Arc::clone(peeks));
    plugin_fn("rocketry", &[TOOLS], move |ctx| {
        let tools = ctx.service::<Tools>(TOOLS)?;
        for (name, replay_safe, calls) in [("launch", false, &launches), ("peek", true, &peeks)] {
            let spec = ToolSpec::new(name, "", r#"{"type":"object"}"#)
                .replay_safe(replay_safe)
                .read_only(replay_safe);
            let calls = Arc::clone(calls);
            let tool = tool_fn(spec, move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok("done".to_owned())
            });
            tools.register(ctx, name, Arc::new(tool))?;
        }
        Ok(())
    })
}

/// A model reply asking for each `(id, tool)` in turn, all in one message.
fn asks(calls: &[(&str, &str)]) -> Message {
    calls
        .iter()
        .fold(Message::assistant(), |message, (id, tool)| {
            message.call(ToolCall::new(*id, *tool, "{}"))
        })
}

/// The events of a run in which the model made `request` and the process then died after
/// `stored` more commits, part of the way through the tools.
async fn interrupted_run(request: Message, stored: usize) -> Vec<Event> {
    // The session's start, the plugin's activation, the prompt, the request to the model
    // and the model's message.
    let log = Failing {
        healthy: 5 + stored,
        commits: Arc::new(AtomicUsize::new(0)),
    };
    let model = Arc::new(ScriptedProvider::new([request]));
    let harness = Harness::resume(model, Arc::new(EventStream::new().with_log(log))).unwrap();
    harness.plugin(rocketry(&Arc::default(), &Arc::default()));
    harness.run("launch it").await.unwrap_err();
    harness.events().events()
}

/// A harness picking up `events`, with fresh counters for its two tools.
fn restarted(
    events: Vec<Event>,
    replies: impl IntoIterator<Item = Message>,
) -> (Harness, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let (launches, peeks) = (Arc::default(), Arc::default());
    let model = Arc::new(ScriptedProvider::new(replies));
    let harness = Harness::resume(model, Arc::new(EventStream::load(events))).unwrap();
    harness.plugin(rocketry(&launches, &peeks));
    (harness, launches, peeks)
}

fn payloads(harness: &Harness, topic: &str) -> Vec<String> {
    harness
        .events()
        .matching(&Pattern::new(topic))
        .into_iter()
        .map(|event| event.payload)
        .collect()
}

#[tokio::test]
async fn a_continued_run_treats_each_call_by_how_far_it_got() {
    // Three calls in one message. The process died while the second was running: the log
    // has the first one's start and result and the second one's start.
    let request = asks(&[("c1", "launch"), ("c2", "launch"), ("c3", "launch")]);
    let events = interrupted_run(request, 3).await;
    let (harness, launches, _peeks) = restarted(events, [say("two of three, perhaps")]);
    let main = SessionId::new(MAIN);

    let refused = harness.run_in(&main, "hello?").await.unwrap_err();
    let outcome = harness.continue_run(&main).await.unwrap();

    assert_eq!(
        refused.to_string(),
        "session `main` has an unfinished run: continue or stop it first"
    );
    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "two of three, perhaps".into()
        }
    );
    // Only the call that never started ran in this process.
    assert_eq!(launches.load(Ordering::SeqCst), 1);
    assert_eq!(payloads(&harness, "run/restart"), [r#"{"attempt":1}"#]);
    assert_eq!(
        payloads(&harness, "tool/result"),
        [
            // Finished before the crash: its result is handed over from the log as it is.
            r#"{"id":"c1","tool":"launch","ok":true,"output":"done"}"#,
            // Running at the crash, and not safe to run twice.
            r#"{"id":"c2","tool":"launch","ok":false,"output":"interrupted: the harness stopped while `launch` was running, and it is not safe to run twice. Check what it did before calling it again."}"#,
            // Never started: run now.
            r#"{"id":"c3","tool":"launch","ok":true,"output":"done"}"#,
        ]
    );
    // The model got all three results, in one message, after its own.
    assert_eq!(outcome.session.conversation().len(), 5);
    assert_eq!(
        payloads(&harness, "tool/start"),
        [
            r#"{"id":"c1","tool":"launch"}"#,
            r#"{"id":"c2","tool":"launch"}"#,
            r#"{"id":"c3","tool":"launch"}"#,
        ]
    );
}

#[tokio::test]
async fn a_call_that_was_running_is_made_again_only_if_its_tool_says_it_is_safe() {
    // The process died while `peek` was running, after `launch` had returned.
    let request = asks(&[("c1", "launch"), ("c2", "peek")]);
    let events = interrupted_run(request, 3).await;
    let (harness, launches, peeks) = restarted(events, [say("launched and looked")]);

    let outcome = harness.continue_run(&SessionId::new(MAIN)).await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "launched and looked".into()
        }
    );
    assert_eq!(
        (
            launches.load(Ordering::SeqCst),
            peeks.load(Ordering::SeqCst)
        ),
        (0, 1)
    );
    assert_eq!(
        payloads(&harness, "tool/result"),
        [
            r#"{"id":"c1","tool":"launch","ok":true,"output":"done"}"#,
            r#"{"id":"c2","tool":"peek","ok":true,"output":"done"}"#,
        ]
    );
}

/// A plugin contributing `snap`, which counts its calls, is not safe to run twice, and
/// returns a caption and an image.
fn camera(shots: &Arc<AtomicUsize>) -> impl Plugin + 'static {
    let shots = Arc::clone(shots);
    plugin_fn("camera", &[TOOLS], move |ctx| {
        let shots = Arc::clone(&shots);
        let snap = tool_fn(
            ToolSpec::new("snap", "", r#"{"type":"object"}"#),
            move |_| {
                shots.fetch_add(1, Ordering::SeqCst);
                Ok(vec![
                    Content::text("the view"),
                    Content::image("image/png", *b"\x89PNG\r\n\x1a\n"),
                ])
            },
        );
        ctx.service::<Tools>(TOOLS)?
            .register(ctx, "snap", Arc::new(snap))?;
        Ok(())
    })
}

/// The blocks of each tool result in the conversation, in order.
fn results(session: &Session) -> Vec<Vec<Content>> {
    session
        .conversation()
        .iter()
        .flat_map(Message::tool_results)
        .map(|result| result.output.content().to_vec())
        .collect()
}

#[tokio::test]
async fn a_finished_image_is_handed_over_from_the_log_and_the_tool_is_not_run_again() {
    // The process died once `snap` had returned and its result was stored, before the
    // message handing that result to the model was: the session's start, the plugin's
    // activation, the prompt, the request to the model, the model's message, the tool's
    // start and its result.
    let log = Failing {
        healthy: 7,
        commits: Arc::new(AtomicUsize::new(0)),
    };
    let model = Arc::new(ScriptedProvider::new([asks(&[("c1", "snap")])]));
    let first = Harness::resume(model, Arc::new(EventStream::new().with_log(log))).unwrap();
    let shots = Arc::default();
    first.plugin(camera(&shots));
    first.run("take a picture").await.unwrap_err();
    assert_eq!(shots.load(Ordering::SeqCst), 1);
    let stored = payloads(&first, "tool/result");
    let events = first.events().events();
    drop(first);

    // A new process, with nothing but the log.
    let again = Arc::default();
    let model = Arc::new(ScriptedProvider::new([say("a fine view")]));
    let second = Harness::resume(model, Arc::new(EventStream::load(events))).unwrap();
    second.plugin(camera(&again));
    let outcome = second.continue_run(&SessionId::new(MAIN)).await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "a fine view".into()
        }
    );
    // The tool did not run in this process, and no second result was recorded.
    assert_eq!(again.load(Ordering::SeqCst), 0);
    assert_eq!(payloads(&second, "tool/result"), stored);
    let recorded: Value = serde_json::from_str(&stored[0]).unwrap();
    assert_eq!(
        recorded,
        json!({
            "id": "c1",
            "tool": "snap",
            "ok": true,
            "output": "the view\n[image: image/png, 8 bytes]",
            "content": [
                {"type": "text", "text": "the view"},
                {"type": "image", "mime_type": "image/png", "data": "iVBORw0KGgo="},
            ],
        })
    );
    // The model was handed the result the tool returned, image included, from the log.
    assert_eq!(
        results(&outcome.session),
        [[
            Content::text("the view"),
            Content::image(
                "image/png",
                [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
            ),
        ]]
    );
}

#[tokio::test]
async fn continuing_a_finished_run_changes_nothing() {
    let (harness, _model) = harness(EventStream::new(), [say("done")]).unwrap();
    let main = SessionId::new(MAIN);
    let first = harness.run("go").await.unwrap();
    let events = harness.events().len();

    let again = harness.continue_run(&main).await.unwrap();

    assert_eq!(again.status, first.status);
    assert_eq!(harness.events().len(), events);
}

#[tokio::test]
async fn a_run_that_keeps_being_restarted_is_given_up() {
    let mut events = interrupted_run(asks(&[("c1", "peek"), ("c2", "launch")]), 1).await;
    let main = SessionId::new(MAIN);
    // Two processes in a row pick the run up and die before finishing a step of it.
    for _ in 0..2 {
        let (harness, _launches, _peeks) = restarted(events, []);
        let harness = harness.with_operation(Arc::new(Broken));
        harness.continue_run(&main).await.unwrap_err();
        events = harness.events().events();
    }
    let (harness, launches, peeks) = restarted(events, [say("never asked")]);
    let harness = harness.with_restart_limit(2);

    let outcome = harness.continue_run(&main).await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Failed {
            reason: "the run was restarted 2 times without finishing and was given up".into()
        }
    );
    assert_eq!(
        payloads(&harness, "run/restart"),
        [r#"{"attempt":1}"#, r#"{"attempt":2}"#, r#"{"attempt":3}"#]
    );
    assert_eq!(
        (
            peeks.load(Ordering::SeqCst),
            launches.load(Ordering::SeqCst)
        ),
        (0, 0)
    );
    // The run is over, so the session takes prompts again.
    assert!(!harness.journal(&main).unwrap().unfinished());
    assert!(harness.run("start over").await.is_ok());
}

/// A step that breaks the pipeline every time, standing in for a crash.
struct Broken;

#[async_trait]
impl Operation for Broken {
    fn name(&self) -> &'static str {
        "broken"
    }

    async fn run(&self, _session: &Session) -> anyhow::Result<Next> {
        anyhow::bail!("the process died")
    }
}
