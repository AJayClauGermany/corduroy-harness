//! The loop is a part that can be swapped: one of one's own, one that wraps the standard one,
//! and one a plugin supplies and takes away again.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use corduroy_harness::core::{SessionId, TOOLS, ToolSpec, Tools, plugin_fn, tool_fn};
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::{
    End, Harness, HarnessError, LOOPS, Loop, Loops, MAIN, Provider, Run, StandardLoop, Status, Step,
};
use serde_json::json;

/// A harness around `model` with a tool, `shout`, that upper-cases its input.
fn harness(model: ScriptedProvider) -> Result<(Harness, Arc<ScriptedProvider>), HarnessError> {
    let model = Arc::new(model);
    let provider: Arc<dyn Provider> = Arc::<ScriptedProvider>::clone(&model);
    let harness = Harness::new(provider)?;
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
    Ok((harness, model))
}

/// Ask the model once, without tools, and take what it says.
struct Once;

#[async_trait]
impl Loop for Once {
    async fn step(&self, run: &Run<'_>) -> Result<Step, HarnessError> {
        let session = run.session();
        if session.model_turns() > 0 {
            return Ok(Step::End(End::settled(&session)));
        }
        run.ask_model(&run.request(&session).with_tools(Vec::new()))
            .await
    }
}

#[tokio::test]
async fn a_loop_of_ones_own_takes_every_step() {
    let (harness, model) = harness(ScriptedProvider::new([
        call("1", "shout", &json!({"text": "hi"})),
        say("never asked"),
    ]))
    .unwrap();
    let harness = harness.with_loop("once");
    harness.loops().insert("once", Arc::new(Once)).unwrap();

    let outcome = harness.run("say hi loudly").await.unwrap();

    // The model was asked once, offered no tools, and its call was never made.
    assert_eq!(outcome.status, Status::Yielded);
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].tools, Vec::<String>::new());
    assert_eq!(harness.events().matching(&"tool/start".into()), []);
    // The end is in the log, as any loop's is.
    assert_eq!(harness.events().matching(&"turn/end".into()).len(), 1);
}

/// The standard loop, noting what each step did.
#[derive(Default)]
struct Watched(Mutex<Vec<Step>>);

#[async_trait]
impl Loop for Watched {
    async fn step(&self, run: &Run<'_>) -> Result<Step, HarnessError> {
        let step = StandardLoop.step(run).await?;
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(step.clone());
        Ok(step)
    }
}

#[tokio::test]
async fn a_loop_can_wrap_the_standard_one() {
    let (harness, _model) = harness(ScriptedProvider::new([
        call("1", "shout", &json!({"text": "hi"})),
        say("done"),
    ]))
    .unwrap();
    let watched = Arc::new(Watched::default());
    let harness = harness.with_loop("watched");
    let as_loop: Arc<dyn Loop> = Arc::<Watched>::clone(&watched);
    harness.loops().insert("watched", as_loop).unwrap();

    let outcome = harness.run("say hi loudly").await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "done".into()
        }
    );
    let steps = watched.0.lock().unwrap().clone();
    assert_eq!(
        steps,
        [
            Step::Applied("model".into()),
            Step::Applied("tools".into()),
            Step::Applied("model".into()),
            Step::End(End::Finished),
        ]
    );
}

#[tokio::test]
async fn a_plugin_supplies_the_loop_and_takes_it_away() {
    let (harness, _model) = harness(ScriptedProvider::new([say("first"), say("second")])).unwrap();
    let harness = harness.with_loop("plugged");
    let plugin = harness.plugin(plugin_fn("plugged-loop", &[LOOPS], |ctx| {
        let loops = ctx.service::<Loops>(LOOPS)?;
        loops.register(ctx, "plugged", Arc::new(Once))?;
        Ok(())
    }));

    let first = harness.run("one").await.unwrap();
    assert_eq!(
        first.status,
        Status::Finished {
            answer: "first".into()
        }
    );

    harness.app().dispose(plugin).unwrap();
    let second = harness.run_in(&SessionId::new(MAIN), "two").await.unwrap();
    assert_eq!(
        second.status,
        Status::Failed {
            reason: "no loop is registered as `plugged`".into()
        }
    );
}
