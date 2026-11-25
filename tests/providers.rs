//! The model as a service: the loop asks whichever provider is registered, and plugins can
//! ask a provider's models for a completion of their own. No network or LLM. The last test
//! uses the `testbed` plugin from `plugins/`; build it first with `cargo plugins`.

use std::sync::Arc;
use std::time::Duration;

use corduroy_harness::core::{
    EventStream, Message, ModelError, ModelInfo, ModelRequest, Pattern, Scope,
};
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::wasm::WasmRuntime;
use corduroy_harness::{Harness, Provider, Status, provider};
use corduroy_test_support::plugin;
use serde_json::json;

/// A request to the model `scripted/small`.
fn asking(text: &str) -> ModelRequest {
    ModelRequest::new(
        "scripted/small",
        "be brief",
        vec![Message::user().text(text)],
    )
}

#[tokio::test]
async fn without_a_provider_a_run_fails_until_a_plugin_supplies_one() {
    let harness = Harness::open(Arc::new(EventStream::new())).unwrap();

    let outcome = harness.run("hello").await.unwrap();

    let Status::Failed { reason } = outcome.status else {
        panic!("expected a failure, got {:?}", outcome.status);
    };
    assert!(
        reason.contains("no model provider is registered as `default`"),
        "{reason}"
    );

    let model = Arc::new(ScriptedProvider::new([say("hi there")]));
    harness.plugin(provider("default", model));
    let outcome = harness.run("hello again").await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "hi there".into()
        }
    );
}

#[tokio::test]
async fn the_loop_asks_the_provider_it_is_told_to_and_follows_a_swap() {
    let harness = Harness::open(Arc::new(EventStream::new()))
        .unwrap()
        .with_provider("main");
    let first = Arc::new(ScriptedProvider::new([say("from the first")]));
    let ignored = Arc::new(ScriptedProvider::new([say("never")]));
    let main: Arc<dyn Provider> = Arc::<ScriptedProvider>::clone(&first);
    let other: Arc<dyn Provider> = Arc::<ScriptedProvider>::clone(&ignored);
    let loaded = harness.plugin(provider("main", main));
    harness.plugin(provider("other", other));

    let outcome = harness.run("one").await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "from the first".into()
        }
    );
    assert_eq!(ignored.requests().len(), 0);

    harness.app().dispose(loaded).unwrap();
    let second = Arc::new(ScriptedProvider::new([say("from the second")]));
    harness.plugin(provider("main", second));
    let outcome = harness.run("two").await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "from the second".into()
        }
    );
    assert_eq!(first.requests().len(), 1);
}

#[tokio::test]
async fn a_provider_serves_its_models_to_plugins_inside_and_outside_the_runtime() {
    let model = Arc::new(ScriptedProvider::new([say("inside"), say("outside")]));
    let harness = Harness::open(Arc::new(EventStream::new())).unwrap();
    let loaded = harness.plugin(provider("scripted", Arc::<ScriptedProvider>::clone(&model)));
    let models = Arc::clone(harness.models());
    let wait = Duration::from_secs(5);
    let said = |request| {
        models
            .complete(request, wait)
            .map(|reply| reply.message.text_content())
    };

    // From a thread of the async runtime, as an event listener is called.
    assert_eq!(said(asking("a")).unwrap(), "inside");
    // From a plain thread, as a tool is called.
    let elsewhere = Arc::clone(&models);
    let answer = std::thread::spawn(move || elsewhere.complete(asking("b"), wait))
        .join()
        .unwrap();
    assert_eq!(answer.unwrap().message.text_content(), "outside");
    let asked = model.requests();
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[0].system, "be brief");
    // Nothing lists a provider's models: it is asked for any of them, by its own name for it.
    assert_eq!(asked[0].model, "small");

    assert!(matches!(said(asking("c")), Err(ModelError::Failed(_))));
    harness.app().dispose(loaded).unwrap();
    assert_eq!(
        said(asking("d")),
        Err(ModelError::Unknown("scripted/small".into()))
    );
}

#[tokio::test]
async fn what_the_host_states_about_a_model_is_laid_over_what_its_provider_reports() {
    let model = Arc::new(ScriptedProvider::new([]));
    let harness = Harness::open(Arc::new(EventStream::new()))
        .unwrap()
        .with_provider("local")
        .with_model("small");
    assert_eq!(harness.model_info(), ModelInfo::unknown());

    let stated = ModelInfo::unknown().with_context(16_384).with_tools(true);
    harness.plugin(provider("local", model).describing("small", stated));

    // The loop and plugins read the same description, each by its own way of asking.
    assert_eq!(harness.model_info(), stated);
    assert_eq!(harness.models().describe("local/small"), Some(stated));
    assert_eq!(
        harness.models().describe("local/other"),
        Some(ModelInfo::unknown())
    );
    assert_eq!(harness.models().describe("elsewhere/small"), None);
}

#[tokio::test]
async fn a_webassembly_plugin_asks_a_model_a_provider_serves_during_a_run() {
    let main = Arc::new(ScriptedProvider::new([
        call(
            "1",
            "ask",
            &json!({"model": "helper/small", "system": "terse", "prompt": "summarise"}),
        ),
        say("done"),
    ]));
    let helper = Arc::new(ScriptedProvider::new([say("a summary")]));
    let harness = Harness::new(main).unwrap();
    harness.plugin(provider("helper", Arc::<ScriptedProvider>::clone(&helper)));
    let mut testbed = WasmRuntime::new().unwrap().load(plugin("testbed")).unwrap();
    for capability in testbed.requires().to_vec() {
        testbed = testbed.grant(capability, Scope::none());
    }
    harness.plugin(testbed.grant("llm", Scope::none().allow("helper/*")));

    let outcome = harness.run("go").await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "done".into()
        }
    );
    let result = harness.events().matching(&Pattern::new("tool/result"));
    let result: serde_json::Value = serde_json::from_str(&result[0].payload).unwrap();
    assert_eq!(result["output"], "\"a summary\"");
    assert_eq!(helper.requests()[0].system, "terse");
}
