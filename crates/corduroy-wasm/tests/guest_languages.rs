//! Plugins written in JavaScript and Python with the SDKs in `sdk/`, loaded by the host like
//! any other. Built by `scripts/guests.sh` from `examples/`; each test skips when its plugin
//! is not built, unless `CORDUROY_REQUIRE_GUESTS` is set (CI sets it).
//!
//! Both languages implement the same `notes` plugin, so the same checks run against each:
//! tools work, a tool's error reaches the host as the right kind, notes are kept in `state`,
//! and the SDK routes `console.log`/`print`, the clock and the random source to host calls.

use std::error::Error;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use corduroy_core::{
    App, EventStream, FiberState, GATES, Gates, HTTP, HttpClients, KERNELS, Kernels, MODELS,
    Models, PROMPTS, Pattern, Prompts, SECRETS, SERVICES, Scope, Secrets, Services, TOOLS,
    ToolError, Tools,
};
use corduroy_test_support::{compile_cache, guest, scratch};
use corduroy_wasm::{WasmPlugin, WasmRuntime};
use serde_json::{Value, json};

struct Host {
    app: App,
    tools: Arc<Tools>,
    prompts: Arc<Prompts>,
}

fn host(events: Arc<EventStream>) -> Result<Host, Box<dyn Error>> {
    let tools = Arc::new(Tools::new());
    let prompts = Arc::new(Prompts::new());
    let mut app = App::with_events(events);
    app.provide(TOOLS, Arc::clone(&tools))?;
    app.provide(PROMPTS, Arc::clone(&prompts))?;
    app.provide(GATES, Arc::new(Gates::new()))?;
    app.provide(SERVICES, Arc::new(Services::new()))?;
    app.provide(KERNELS, Arc::new(Kernels::new()))?;
    app.provide(MODELS, Arc::new(Models::new()))?;
    app.provide(SECRETS, Arc::new(Secrets::new()))?;
    app.provide(HTTP, Arc::new(HttpClients::new()))?;
    Ok(Host {
        app,
        tools,
        prompts,
    })
}

/// The notes plugin built from `component`, with the capabilities it was built with.
fn notes(dir: &Path, component: &Path) -> Result<WasmPlugin, Box<dyn Error>> {
    Ok(WasmRuntime::cached(compile_cache())?
        .with_root(dir)
        .load(component)?
        .named("notes")
        .grant("state", Scope::any())
        .grant("log", Scope::any())
        .grant("clock", Scope::any())
        .grant("random", Scope::any()))
}

fn call(host: &Host, tool: &str, input: &Value) -> Result<String, ToolError> {
    host.tools.call(tool, &input.to_string())
}

/// The current year, near enough.
fn this_year() -> u64 {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    1970 + seconds / 31_556_952
}

/// What `note_list` returns, parsed.
fn listed(host: &Host) -> Result<Vec<Value>, Box<dyn Error>> {
    Ok(serde_json::from_str(&call(host, "note_list", &json!({}))?)?)
}

/// A test that the notes plugin built as `$guest` passes the checks every language must:
/// its tools and their flags, errors of the right kind, host randomness and time, its log
/// lines on the stream, and notes kept in `state` across a reload.
macro_rules! notes_plugin_works {
    ($test:ident, $guest:literal) => {
        #[test]
        fn $test() {
            let Some(component) = guest($guest) else {
                return;
            };
            let dir = scratch($guest);
            let events = Arc::new(EventStream::new());
            let mut host = host(Arc::clone(&events)).unwrap();
            let id = host.app.plugin(notes(&dir, &component).unwrap());
            assert_eq!(host.app.state(id), Some(&FiberState::Active));

            // The tools, with the flags each declared.
            let flags: Vec<(String, bool, bool)> = host
                .tools
                .specs()
                .into_iter()
                .map(|spec| (spec.name, spec.read_only, spec.replay_safe))
                .collect();
            assert_eq!(
                flags,
                [
                    ("note_add".to_owned(), false, false),
                    ("note_list".to_owned(), true, true),
                    ("note_remove".to_owned(), false, false)
                ]
            );
            assert!(host.prompts.render().is_empty(), "no notes, no prompt part");

            // Ids come from the host's random source, so two differ; times from its clock.
            let first = call(&host, "note_add", &json!({"text": "buy milk"})).unwrap();
            let second = call(&host, "note_add", &json!({"text": "call home"})).unwrap();
            assert_eq!(first.len(), 8);
            assert_ne!(first, second);
            let saved = listed(&host).unwrap();
            assert_eq!(saved.len(), 2);
            let at = saved[0]["at"].as_str().unwrap();
            let year: u64 = at.get(..4).unwrap().parse().unwrap();
            assert!(year.abs_diff(this_year()) <= 1, "the clock said {at}");

            // A tool's own error and bad input come back as the right kinds.
            assert!(matches!(
                call(&host, "note_add", &json!({"text": ""})),
                Err(ToolError::InvalidInput(_))
            ));
            assert!(matches!(
                host.tools.call("note_add", "not json"),
                Err(ToolError::InvalidInput(_))
            ));
            assert!(matches!(
                call(&host, "note_remove", &json!({"id": "nope"})),
                Err(ToolError::InvalidInput(_))
            ));
            let removed = call(&host, "note_remove", &json!({"id": first})).unwrap();
            assert_eq!(removed, format!("removed {first}"));

            // The prompt part counts what is left, and the plugin's own log line was recorded.
            let rendered = host.prompts.render();
            assert!(rendered[0].1.contains("1 saved note"), "{rendered:?}");
            let logs = events.matching(&Pattern::new("plugin/log"));
            assert!(
                logs.iter().any(|event| event.payload.contains("saved note")),
                "{logs:?}"
            );

            // Notes are in `state`, so the plugin loaded again finds them.
            host.app.dispose(id).unwrap();
            let again = host.app.plugin(notes(&dir, &component).unwrap());
            assert_eq!(host.app.state(again), Some(&FiberState::Active));
            assert_eq!(listed(&host).unwrap()[0]["id"], json!(second));
        }
    };
}

notes_plugin_works!(
    a_javascript_plugin_built_with_the_sdk_works_in_the_host,
    "notes_js"
);

/// A plugin is instantiated only with every capability its world imports, and the world is
/// what the SDK generated from the capability list: nothing more.
#[test]
fn a_javascript_plugin_imports_only_the_capabilities_it_was_built_with() {
    let Some(component) = guest("notes_js") else {
        return;
    };
    let plugin = WasmRuntime::cached(compile_cache())
        .unwrap()
        .load(component)
        .unwrap();
    assert_eq!(plugin.requires(), ["log", "clock", "random", "state"]);
}
