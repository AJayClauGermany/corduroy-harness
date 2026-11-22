//! Loading plugins safely: compiled code cached, the host's files protected, plugins loading
//! plugins with no more than they hold and only as the gates allow, and cascades cut off.
//! Uses the plugins in `plugins/`; build them first with `cargo plugins`.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use corduroy_core::{
    App, Draft, EventStream, FiberState, GATES, Gate, Gates, HTTP, HttpClients, KERNELS, Kernels,
    MODELS, Models, PLUGIN_LOAD, PROMPTS, Pattern, Prompts, SECRETS, SERVICES, Scope, Secrets,
    Source, TOOLS, ToolError, Tools, Verdict,
};
use corduroy_shell::ShellKernel;
use corduroy_test_support::{plugin, scratch};
use corduroy_wasm::{Limits, WasmPlugin, WasmRuntime};
use serde_json::{Value, json};

struct Host {
    app: App,
    tools: Arc<Tools>,
    gates: Arc<Gates>,
}

fn host(dir: &Path) -> Result<Host, Box<dyn Error>> {
    let tools = Arc::new(Tools::new());
    let gates = Arc::new(Gates::new());
    let kernels = Arc::new(Kernels::new());
    kernels.insert(
        "shell",
        Arc::new(std::sync::Mutex::new(ShellKernel::new().in_dir(dir))),
    )?;
    let mut app = App::with_events(Arc::new(EventStream::new()));
    app.provide(TOOLS, Arc::clone(&tools))?;
    app.provide(PROMPTS, Arc::new(Prompts::new()))?;
    app.provide(GATES, Arc::clone(&gates))?;
    app.provide(SERVICES, Arc::new(corduroy_core::Services::new()))?;
    app.provide(KERNELS, kernels)?;
    app.provide(MODELS, Arc::new(Models::new()))?;
    app.provide(SECRETS, Arc::new(Secrets::new()))?;
    app.provide(HTTP, Arc::new(HttpClients::new()))?;
    Ok(Host { app, tools, gates })
}

/// The directory the built plugins are in.
fn built() -> PathBuf {
    plugin("testbed")
        .parent()
        .map_or_else(PathBuf::new, Path::to_path_buf)
}

/// The testbed as `loader`, allowed to load plugins named `child-*` from the build directory
/// and to pass on reading `work`. Every other capability it imports is linked empty.
fn loader(runtime: &WasmRuntime, work: &Path) -> Result<WasmPlugin, Box<dyn Error>> {
    let readable = Scope::subtree(&built().to_string_lossy())
        .allow(work.to_string_lossy().into_owned())
        .allow(format!("{}/*", work.to_string_lossy()));
    let mut testbed = runtime
        .load(plugin("testbed"))?
        .named("loader")
        .config(json!({"prefix": "l."}).to_string())
        .grant("plugins", Scope::none().allow("child-*"))
        .grant("fs-read", readable);
    for capability in [
        "events", "log", "clock", "random", "tools", "state", "service", "llm", "secret", "http",
        "log-read",
    ] {
        testbed = testbed.grant(capability, Scope::none());
    }
    Ok(testbed)
}

/// Have the loader load `path` as `name` with `grants`, and parse what it says.
fn load(host: &Host, path: &Path, name: &str, grants: &Value) -> Result<Value, Box<dyn Error>> {
    let input = json!({"path": path, "name": name, "grants": grants});
    Ok(serde_json::from_str(
        &host.tools.call("l.load", &input.to_string())?.to_text(),
    )?)
}

/// `pi-tools` asks for these: every path to read, nothing to write, no kernel.
fn greedy() -> Value {
    json!([
        {"capability": "fs-read", "allow": ["*"], "deny": []},
        {"capability": "fs-write", "allow": [], "deny": []},
        {"capability": "kernel", "allow": [], "deny": []},
    ])
}

#[test]
fn a_component_is_compiled_once_per_runtime() {
    let runtime = WasmRuntime::new().unwrap();

    let shared = runtime.clone();
    let first = runtime.load(plugin("approval")).unwrap();
    let again = shared.load(plugin("approval")).unwrap();
    assert_eq!(runtime.components(), 1);
    runtime.load(plugin("testbed")).unwrap();

    // Clones share what was compiled.
    assert_eq!(shared.components(), 2);
    assert_eq!(first.hash(), again.hash());
    assert_eq!(first.hash().len(), 64);
}

#[test]
fn compiled_code_is_kept_on_disk_where_no_plugin_reaches() {
    let dir = scratch("disk-cache").join("cache");
    let runtime = WasmRuntime::cached(&dir).unwrap();

    runtime.load(plugin("approval")).unwrap();

    // The cache is written as the component is compiled; give a slow disk a moment.
    let deadline = Instant::now() + Duration::from_secs(5);
    while files(&dir) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(files(&dir) > 0);
    assert!(runtime.protected().contains(&dir.canonicalize().unwrap()));
    let host = std::env::current_exe().unwrap().canonicalize().unwrap();
    assert!(runtime.protected().contains(&host));
}

/// How many files are below `dir`.
fn files(dir: &Path) -> usize {
    fs::read_dir(dir).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .map(|entry| {
                let path = entry.path();
                if path.is_dir() { files(&path) } else { 1 }
            })
            .sum()
    })
}

#[test]
fn protected_paths_are_out_of_reach_whatever_the_grant() {
    let dir = scratch("protected");
    fs::create_dir_all(dir.join("vault")).unwrap();
    fs::write(dir.join("vault/key.txt"), "secret").unwrap();
    fs::write(dir.join("notes.txt"), "fine").unwrap();
    let mut host = host(&dir).unwrap();
    let everything = Scope::subtree(&dir.to_string_lossy());
    let tools = WasmRuntime::new()
        .unwrap()
        .with_root(&dir)
        .protect(dir.join("vault"))
        .load(plugin("pi-tools"))
        .unwrap()
        .grant("fs-read", everything.clone())
        .grant("fs-write", everything)
        .grant("kernel", Scope::none());
    host.app.plugin(tools);

    let notes = host.tools.call("read", r#"{"path":"notes.txt"}"#);
    let key = host.tools.call("read", r#"{"path":"vault/key.txt"}"#);
    let planted = host
        .tools
        .call("write", r#"{"path":"vault/new.txt","content":"x"}"#);

    assert_eq!(notes.unwrap().to_text(), "fine");
    assert!(
        matches!(&key, Err(ToolError::Denied(reason)) if reason.contains("protected by the host")),
        "{key:?}"
    );
    assert!(matches!(planted, Err(ToolError::Denied(_))), "{planted:?}");
    assert!(!dir.join("vault/new.txt").exists());
}

#[test]
fn a_plugin_loads_children_with_no_more_than_it_holds() {
    let work = scratch("load-children");
    fs::write(work.join("a.txt"), "in reach").unwrap();
    let mut host = host(&work).unwrap();
    let runtime = WasmRuntime::new().unwrap().with_root(&work);
    let parent = host.app.plugin(loader(&runtime, &work).unwrap());

    let loaded = load(&host, &plugin("pi-tools"), "child-tools", &greedy()).unwrap();
    // The child exists once the tree settles, not during the loader's call.
    assert!(host.tools.get("read").is_none());
    assert!(host.app.apply_requests());

    assert_eq!(loaded.as_str().map(str::len), Some(64));
    let inside = host
        .tools
        .call("read", &json!({"path": work.join("a.txt")}).to_string());
    let outside = host.tools.call("read", r#"{"path":"/etc/hostname"}"#);
    assert_eq!(inside.unwrap().to_text(), "in reach");
    // It asked for every path, and got only those its loader can read.
    assert!(matches!(outside, Err(ToolError::Denied(_))), "{outside:?}");
    assert_eq!(
        host.tools.call("l.loaded", "{}").unwrap().to_text(),
        r#"["child-tools"]"#
    );

    // Unloading the loader takes its children with it.
    host.app.dispose(parent).unwrap();
    assert!(host.tools.get("read").is_none());
}

#[test]
fn a_load_beyond_the_loaders_own_grants_is_refused() {
    let work = scratch("load-refused");
    let mut host = host(&work).unwrap();
    let runtime = WasmRuntime::new().unwrap().with_root(&work);
    host.app.plugin(loader(&runtime, &work).unwrap());
    let writer = json!([
        {"capability": "fs-read", "allow": [], "deny": []},
        {"capability": "fs-write", "allow": ["*"], "deny": []},
        {"capability": "kernel", "allow": [], "deny": []},
    ]);

    let amplified = load(&host, &plugin("pi-tools"), "child-writer", &writer).unwrap();
    let misnamed = load(&host, &plugin("pi-tools"), "other", &greedy()).unwrap();
    let unreadable = load(&host, Path::new("/etc/hostname"), "child-x", &json!([])).unwrap();
    let ungranted = load(&host, &plugin("pi-tools"), "child-bare", &json!([])).unwrap();

    assert_eq!(
        amplified["denied"],
        "cannot grant `fs-write`: the loader does not hold it, so it can grant it only with an \
         empty scope"
    );
    assert_eq!(
        misnamed["denied"],
        "capability `plugins` does not cover `other`"
    );
    assert_eq!(
        unreadable["denied"],
        "capability `fs-read` does not cover `/etc/hostname`"
    );
    assert_eq!(
        ungranted["failed"],
        "`child-bare` imports fs-read, fs-write, kernel, which the request does not grant"
    );
    assert!(!host.app.apply_requests());
}

#[test]
fn every_load_is_asked_of_the_gates_first() {
    let work = scratch("load-gated");
    let mut host = host(&work).unwrap();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let noted = Arc::clone(&seen);
    let gate: Arc<dyn Gate> = Arc::new(move |point: &str, request: &str| {
        if point != PLUGIN_LOAD {
            return None;
        }
        let request: Value = serde_json::from_str(request).ok()?;
        noted.lock().ok()?.push(request.clone());
        Some(match request["name"].as_str()? {
            "child-asked" => Verdict::Ask("new code".into()),
            "child-denied" => Verdict::Deny("not this one".into()),
            _ => Verdict::Allow,
        })
    });
    host.gates.insert("vetting", gate).unwrap();
    let runtime = WasmRuntime::new().unwrap().with_root(&work);
    host.app.plugin(loader(&runtime, &work).unwrap());

    let denied = load(&host, &plugin("pi-tools"), "child-denied", &greedy()).unwrap();
    let asked = load(&host, &plugin("pi-tools"), "child-asked", &greedy()).unwrap();
    let allowed = load(&host, &plugin("pi-tools"), "child-allowed", &greedy()).unwrap();

    assert_eq!(denied["denied"], "denied by gate `vetting`: not this one");
    assert!(
        asked["denied"]
            .as_str()
            .unwrap()
            .contains("which a plugin's call cannot wait for: new code")
    );
    assert!(allowed.is_string());
    let request = &seen.lock().unwrap()[0];
    assert_eq!(request["loader"], "plugin:loader");
    assert_eq!(request["hash"].as_str().map(str::len), Some(64));
    assert_eq!(
        request["requires"],
        json!(["fs-read", "fs-write", "kernel"])
    );
    assert_eq!(request["grants"]["fs-read"]["allow"], json!(["*"]));
    let decisions = host.app.events().matching(&Pattern::new("gate/decision"));
    assert_eq!(decisions.len(), 3);
    assert!(decisions[0].payload.contains(r#""point":"plugin/load""#));
    assert_eq!(
        host.app
            .events()
            .matching(&Pattern::new("plugin/requested"))
            .len(),
        1
    );
}

#[test]
fn a_plugin_may_not_gate_loads_and_make_them() {
    let work = scratch("load-gate-excl");
    let mut host = host(&work).unwrap();
    let runtime = WasmRuntime::new().unwrap();
    let gatekeeper = loader(&runtime, &work)
        .unwrap()
        .config(json!({"gates": ["plugin/load"]}).to_string());

    let id = host.app.plugin(gatekeeper);

    let Some(FiberState::Failed(reason)) = host.app.state(id) else {
        panic!("a plugin gating loads must not also load");
    };
    assert!(
        reason.contains("gates `plugin/load` and imports `plugins`"),
        "{reason}"
    );
}

/// A testbed as `name` that emits `emit` for every `on` it hears, with chains of causes cut
/// at `cascade`.
fn echo(name: &str, on: &str, emit: &str, cascade: usize) -> Result<WasmPlugin, Box<dyn Error>> {
    let limits = Limits {
        cascade,
        ..Limits::default()
    };
    let config = json!({"prefix": format!("{name}."), "echo": {"on": on, "emit": emit}});
    let mut testbed = WasmRuntime::new()?
        .with_limits(limits)
        .load(plugin("testbed"))?
        .named(name)
        .config(config.to_string())
        .grant("events", Scope::any());
    for capability in [
        "log", "clock", "random", "tools", "state", "service", "plugins", "llm", "secret", "http",
        "log-read",
    ] {
        testbed = testbed.grant(capability, Scope::none());
    }
    Ok(testbed)
}

#[test]
fn plugins_reacting_to_each_other_are_stopped_at_the_cascade_limit() {
    let work = scratch("cascade");
    let mut host = host(&work).unwrap();
    host.app.plugin(echo("pinger", "pong", "ping", 8).unwrap());
    host.app.plugin(echo("ponger", "ping", "pong", 8).unwrap());

    host.app
        .events()
        .emit(Draft::new(Source::Host, "ping"))
        .unwrap();

    let events = host.app.events();
    let volleys =
        events.matching(&Pattern::new("ping")).len() + events.matching(&Pattern::new("pong")).len();
    let stopped = host.app.events().matching(&Pattern::new("plugin/cascade"));
    // The first ping, then one event for each link of the chain up to the limit.
    assert_eq!(volleys, 9, "{volleys}");
    assert_eq!(stopped.len(), 1);
    assert!(stopped[0].cause.is_some());
}

#[test]
fn a_plugin_listening_to_everything_is_not_set_off_by_the_cascade_record() {
    let work = scratch("cascade-watcher");
    let mut host = host(&work).unwrap();
    host.app.plugin(echo("pinger", "pong", "ping", 4).unwrap());
    host.app.plugin(echo("ponger", "ping", "pong", 4).unwrap());
    // Hears every event, `plugin/cascade` included, and emits nothing.
    host.app.plugin(echo("watcher", "*", "", 4).unwrap());

    host.app
        .events()
        .emit(Draft::new(Source::Host, "ping"))
        .unwrap();

    let events = host.app.events();
    let stopped = events.matching(&Pattern::new("plugin/cascade"));
    assert_ne!(stopped.len(), 0);
    // Each record answers an event of the chain, never another record.
    for record in &stopped {
        let cause = events.get(record.cause.unwrap()).unwrap();
        assert_ne!(cause.topic, "plugin/cascade");
    }
}
