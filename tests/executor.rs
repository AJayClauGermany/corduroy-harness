//! The loop runs on any executor: here, a dozen lines that park the thread until woken, with
//! no tokio runtime anywhere. Tools then run where the loop runs, and a stop wakes a model
//! request that would otherwise never end.

use std::pin::pin;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

use async_trait::async_trait;
use corduroy_harness::core::{Message, SessionId, TOOLS, ToolSpec, Tools, plugin_fn, tool_fn};
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::{Harness, MAIN, ModelReply, ModelRequest, Provider, ProviderError, Status};
use serde_json::json;

/// Wakes the thread that is waiting in [`block_on`].
struct Unpark(Thread);

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// Poll `future` on this thread until it is ready, sleeping until something wakes it.
fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        thread::park();
    }
}

#[test]
fn a_run_with_a_tool_call_needs_no_runtime() {
    let model = ScriptedProvider::new([call("1", "shout", &json!({"text": "hi"})), say("done")]);
    let harness = Harness::new(Arc::new(model)).unwrap();
    harness.plugin(plugin_fn("shouter", &[TOOLS], |ctx| {
        let spec = ToolSpec::new("shout", "Upper-case the input.", r#"{"type":"object"}"#);
        let tools = ctx.service::<Tools>(TOOLS)?;
        tools.register(
            ctx,
            "shout",
            Arc::new(tool_fn(spec, |input| Ok(input.to_uppercase()))),
        )?;
        Ok(())
    }));

    let outcome = block_on(harness.run("say hi loudly")).unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "done".into()
        }
    );
    let shouted = outcome
        .session
        .conversation()
        .iter()
        .flat_map(Message::tool_results)
        .any(|result| result.ok && result.output.to_text().contains("HI"));
    assert!(shouted, "the tool's result is in the conversation");
}

/// A model that says when it was asked, and then never answers.
struct Silent(Mutex<Sender<()>>);

#[async_trait]
impl Provider for Silent {
    async fn complete(&self, _request: &ModelRequest) -> Result<ModelReply, ProviderError> {
        if let Ok(asked) = self.0.lock() {
            asked.send(()).unwrap_or_default();
        }
        std::future::pending().await
    }
}

#[test]
fn a_stop_wakes_a_run_waiting_for_the_model() {
    let (asked, model_was_asked) = mpsc::channel();
    let harness = Arc::new(Harness::new(Arc::new(Silent(Mutex::new(asked)))).unwrap());
    let stopper = Arc::clone(&harness);
    let stopping = thread::spawn(move || {
        model_was_asked.recv().unwrap();
        stopper.stop(&SessionId::new(MAIN)).unwrap()
    });

    let outcome = block_on(harness.run("think forever")).unwrap();

    assert!(
        stopping.join().unwrap(),
        "the run was going when it was stopped"
    );
    assert_eq!(outcome.status, Status::Stopped);
}
