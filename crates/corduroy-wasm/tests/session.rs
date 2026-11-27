//! The `session` capability: a plugin learns which session it is being called for, and
//! whether the run it serves was asked to stop. Uses the plugins in `plugins/`; build them
//! first with `cargo plugins`.

use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use corduroy_core::call::{Serve, within};
use corduroy_core::{
    App, Draft, EventStream, GATES, Gates, HTTP, HttpClients, KERNELS, Kernels, MODELS, Models,
    PROMPTS, Pattern, Prompts, SECRETS, SERVICES, Scope, Secrets, Services, SessionId, Source,
    TOOLS, Tools,
};
use corduroy_test_support::plugin;
use corduroy_wasm::{WasmPlugin, WasmRuntime};
use serde_json::{Value, json};

struct Host {
    app: App,
    tools: Arc<Tools>,
}

fn host() -> Result<Host, Box<dyn Error>> {
    let tools = Arc::new(Tools::new());
    let mut app = App::with_events(Arc::new(EventStream::new()));
    app.provide(TOOLS, Arc::clone(&tools))?;
    app.provide(PROMPTS, Arc::new(Prompts::new()))?;
    app.provide(GATES, Arc::new(Gates::new()))?;
    app.provide(SERVICES, Arc::new(Services::new()))?;
    app.provide(KERNELS, Arc::new(Kernels::new()))?;
    app.provide(MODELS, Arc::new(Models::new()))?;
    app.provide(SECRETS, Arc::new(Secrets::new()))?;
    app.provide(HTTP, Arc::new(HttpClients::new()))?;
    Ok(Host { app, tools })
}

/// `name` from `plugins/`, with every capability it imports linked empty but for `grants`.
fn load(name: &str, grants: &[(&str, Scope)]) -> Result<WasmPlugin, Box<dyn Error>> {
    let mut loaded = WasmRuntime::new()?.load(plugin(name))?;
    for capability in loaded.requires().to_vec() {
        loaded = loaded.grant(capability, Scope::none());
    }
    for (capability, scope) in grants {
        loaded = loaded.grant(*capability, scope.clone());
    }
    Ok(loaded)
}

/// What the testbed's `session` tool reports.
fn asked(host: &Host) -> Result<Value, Box<dyn Error>> {
    Ok(serde_json::from_str(
        &host.tools.call("session", "{}")?.to_text(),
    )?)
}

/// Call `tool` with `input` as the tool call `call` of `session`.
fn call_in(host: &Host, session: &str, call: &str, tool: &str, input: &Value) -> String {
    let serve = || host.tools.call(tool, &input.to_string());
    let (result, _waits) = within(SessionId::new(session), call.to_owned(), serve);
    result.map_or_else(|error| error.to_string(), |output| output.to_text())
}

#[test]
fn a_plugin_learns_the_session_and_call_it_serves() {
    let mut host = host().unwrap();
    host.app
        .plugin(load("testbed", &[("session", Scope::any())]).unwrap());

    let outside = asked(&host).unwrap();
    let (inside, _) = within(SessionId::new("main/1"), "call-7".into(), || asked(&host));
    let (rendering, _) = Serve::session(SessionId::new("main")).run(|| asked(&host));

    assert_eq!(
        outside,
        json!({"current": null, "call": null, "stopping": false, "budget": null})
    );
    assert_eq!(
        inside.unwrap(),
        json!({"current": "main/1", "call": "call-7", "stopping": false, "budget": null})
    );
    assert_eq!(
        rendering.unwrap(),
        json!({"current": "main", "call": null, "stopping": false, "budget": null})
    );
    // A call that was told what its result may take can ask.
    let bounded = Serve::session(SessionId::new("main"))
        .call("call-8")
        .budget(2048);
    let (told, _) = bounded.run(|| asked(&host));
    assert_eq!(told.unwrap()["budget"], 2048);
}

#[test]
fn a_session_the_grant_does_not_reach_is_refused_and_recorded() {
    let mut host = host().unwrap();
    host.app
        .plugin(load("testbed", &[("session", Scope::none().allow("main"))]).unwrap());

    let (main, _) = within(SessionId::new("main"), "1".into(), || asked(&host));
    let (child, _) = within(SessionId::new("main/1"), "2".into(), || asked(&host));

    assert_eq!(main.unwrap()["current"], "main");
    let child = child.unwrap();
    assert!(child["current"]["denied"].is_string(), "{child}");
    assert!(child["call"]["denied"].is_string(), "{child}");
    let denied = host
        .app
        .events()
        .matching(&Pattern::new("capability/denied"));
    assert_eq!(
        denied.first().map(|event| event.payload.as_str()),
        // With the call it was serving, which is the one the scope names.
        Some(r#"{"plugin":"testbed","capability":"session","resource":"main/1","call":"2"}"#)
    );
}

#[test]
fn a_plugin_learns_that_the_run_it_serves_is_stopping() {
    let mut host = host().unwrap();
    host.app
        .plugin(load("testbed", &[("session", Scope::any())]).unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let serve = || {
        Serve::session(SessionId::new("main"))
            .call("1")
            .stopped_by(Arc::clone(&stop))
    };

    let (before, _) = serve().run(|| asked(&host));
    stop.store(true, Ordering::SeqCst);
    let (after, _) = serve().run(|| asked(&host));

    assert_eq!(before.unwrap()["stopping"], false);
    assert_eq!(after.unwrap()["stopping"], true);
}

#[test]
fn an_event_of_a_session_is_handled_for_that_session() {
    let mut host = host().unwrap();
    let config = json!({"echo": {"on": "ping", "emit": "pong", "session": true}});
    let echo = load(
        "testbed",
        &[("session", Scope::any()), ("events", Scope::any())],
    )
    .unwrap()
    .config(config.to_string());
    host.app.plugin(echo);
    let events = Arc::clone(host.app.events());

    events
        .emit(Draft::new(Source::User, "ping").session(SessionId::new("main/2")))
        .unwrap();
    events.emit(Draft::new(Source::User, "ping")).unwrap();

    let pongs = events.matching(&Pattern::new("pong"));
    let seen: Vec<(Option<&str>, &str)> = pongs
        .iter()
        .map(|event| {
            let session = event.session.as_ref().map(SessionId::as_str);
            (session, event.payload.as_str())
        })
        .collect();
    // The echo of the session's event is the session's too; the other belongs to none.
    assert_eq!(
        seen,
        [
            (Some("main/2"), r#"{"session":"main/2"}"#),
            (None, r#"{"session":null}"#)
        ]
    );
}

#[test]
fn the_todo_plugin_keeps_a_list_for_each_session() {
    let mut host = host().unwrap();
    let grants = [("state", Scope::any()), ("session", Scope::any())];
    host.app.plugin(load("todo", &grants).unwrap());
    let write = |session: &str, content: &str| {
        let input = json!({"content": content});
        call_in(&host, session, "w", "todo_write", &input)
    };
    let read = |session: &str| call_in(&host, session, "r", "todo_read", &json!({}));

    write("main", "- [ ] the parent's plan");
    write("main/1", "- [ ] the sub-agent's plan");

    assert_eq!(read("main"), "- [ ] the parent's plan");
    assert_eq!(read("main/1"), "- [ ] the sub-agent's plan");
    assert_eq!(read("main/2"), "(the list is empty)");

    // Each session's list is in its prompt, and in no other's.
    let prompts = host.app.service::<Prompts>(PROMPTS).unwrap();
    let render = |session: &str| {
        let (parts, _) = Serve::session(SessionId::new(session)).run(|| prompts.render());
        parts.into_iter().map(|(_, text)| text).collect::<String>()
    };
    assert!(render("main").contains("the parent's plan"));
    assert!(!render("main").contains("sub-agent"));
    assert!(render("main/1").contains("the sub-agent's plan"));
    assert_eq!(render("main/2"), "");

    // Clearing one list leaves the other.
    write("main/1", "");
    assert_eq!(read("main/1"), "(the list is empty)");
    assert_eq!(read("main"), "- [ ] the parent's plan");
}
