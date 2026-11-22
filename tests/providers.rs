//! The model as a service: the loop asks whichever provider is registered, and plugins can
//! ask a provider's models for a completion of their own. No network or LLM. The last test
//! uses the `testbed` plugin from `plugins/`; build it first with `cargo plugins`.

use std::sync::Arc;
use std::time::Duration;

use corduroy_harness::core::{Completion, EventStream, ModelError, Pattern, Role, Scope, Turn};
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::wasm::WasmRuntime;
use corduroy_harness::{Harness, Provider, Status, provider};
use corduroy_test_support::plugin;
use serde_json::json;

fn asking(text: &str) -> Completion {
    Completion {
        system: "be brief".into(),
        messages: vec![Turn {
            role: Role::User,
            text: text.into(),
        }],
    }
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
    let loaded = harness
        .plugin(provider("scripted", Arc::<ScriptedProvider>::clone(&model)).serving(["small"]));
    let models = Arc::clone(harness.models());
    let wait = Duration::from_secs(5);

    // From a thread of the async runtime, as an event listener is called.
    assert_eq!(
        models.complete("small", &asking("a"), wait).unwrap(),
        "inside"
    );
    // From a plain thread, as a tool is called.
    let elsewhere = Arc::clone(&models);
    let answer = std::thread::spawn(move || elsewhere.complete("small", &asking("b"), wait))
        .join()
        .unwrap();
    assert_eq!(answer.unwrap(), "outside");
    let asked = model.requests();
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[0].system, "be brief");

    assert!(matches!(
        models.complete("small", &asking("c"), wait),
        Err(ModelError::Failed(_))
    ));
    harness.app().dispose(loaded).unwrap();
    assert_eq!(
        models.complete("small", &asking("d"), wait),
        Err(ModelError::Unknown("small".into()))
    );
}

#[tokio::test]
async fn a_webassembly_plugin_asks_a_model_a_provider_serves_during_a_run() {
    let main = Arc::new(ScriptedProvider::new([
        call(
            "1",
            "ask",
            &json!({"model": "small", "system": "terse", "prompt": "summarise"}),
        ),
        say("done"),
    ]));
    let helper = Arc::new(ScriptedProvider::new([say("a summary")]));
    let harness = Harness::new(main).unwrap();
    harness.plugin(provider("helper", Arc::<ScriptedProvider>::clone(&helper)).serving(["small"]));
    let mut testbed = WasmRuntime::new().unwrap().load(plugin("testbed")).unwrap();
    for capability in testbed.requires().to_vec() {
        testbed = testbed.grant(capability, Scope::none());
    }
    harness.plugin(testbed.grant("llm", Scope::none().allow("small")));

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
