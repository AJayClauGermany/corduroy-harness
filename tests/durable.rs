//! Runs that outlive the process: the log on disk, and picking a run up from it.
//! No WebAssembly, network or LLM.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use corduroy_harness::core::{
    Context, Event, EventStream, Log, LogError, PluginError, TOOLS, ToolSpec, Tools, plugin_fn,
    tool_fn,
};
use corduroy_harness::oi_kit_provider_types::conversation::message::Message;
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::{Harness, HarnessError, MAIN, Status, jsonl};
use corduroy_test_support::scratch;
use serde_json::json;

/// A native plugin contributing `echo`, which returns its input.
fn echo(ctx: &mut Context<'_>) -> Result<(), PluginError> {
    let spec = ToolSpec {
        name: "echo".into(),
        description: "Return the input.".into(),
        input_schema: r#"{"type":"object"}"#.into(),
    };
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
    let mut harness = Harness::resume(provider, Arc::new(events))?;
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

    // The prompt, the model's call (message, tool/call and usage together), the tool's
    // result (message and tool/result together), the answer, and the end of the turn.
    assert_eq!(lines(&path) - setup, 5);
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

#[tokio::test]
async fn a_step_the_log_refuses_ends_the_run_before_anyone_sees_it() {
    let commits = Arc::new(AtomicUsize::new(0));
    // Room for the session's start, the plugin's activation, the prompt and the model's
    // call, but not the result.
    let log = Failing {
        healthy: 4,
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
    assert_eq!(harness.events().len(), 6);
    let journal = harness.journal(&MAIN.into()).unwrap();
    assert_eq!(journal.session().conversation().len(), 2);
    assert_eq!(model.requests().len(), 1);
}
