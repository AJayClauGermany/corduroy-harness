//! Plugins calling each other: tools through the gates, services, and the `state` store. Uses
//! the `testbed` plugin from `plugins/`; build it first with `cargo plugins`.

use std::error::Error;
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use corduroy_core::{
    App, CallError, EventStream, FiberState, GATES, Gate, Gates, KERNELS, Kernels, PROMPTS,
    Pattern, Prompts, SERVICES, Scope, Service, Services, TOOLS, Tool, ToolSpec, Tools, Verdict,
    tool_fn,
};
use corduroy_test_support::{plugin, scratch};
use corduroy_wasm::{Limits, WasmPlugin, WasmRuntime};
use serde_json::{Value, json};

struct Host {
    app: App,
    tools: Arc<Tools>,
    gates: Arc<Gates>,
    services: Arc<Services>,
}

/// An app providing every registry, with two native tools: `peek`, which only reads, and
/// `poke`, which does not. Each returns its own name.
fn host(events: EventStream) -> Result<Host, Box<dyn Error>> {
    let tools = Arc::new(Tools::new());
    let gates = Arc::new(Gates::new());
    let services = Arc::new(Services::new());
    for (name, read_only) in [("peek", true), ("poke", false)] {
        let spec = ToolSpec {
            name: name.into(),
            description: String::new(),
            input_schema: r#"{"type":"object"}"#.into(),
            replay_safe: true,
            read_only,
        };
        tools.insert(name, Arc::new(tool_fn(spec, move |_| Ok(name.to_owned()))))?;
    }
    let mut app = App::with_events(Arc::new(events));
    app.provide(TOOLS, Arc::clone(&tools))?;
    app.provide(PROMPTS, Arc::new(Prompts::new()))?;
    app.provide(GATES, Arc::clone(&gates))?;
    app.provide(SERVICES, Arc::clone(&services))?;
    app.provide(KERNELS, Arc::new(Kernels::new()))?;
    Ok(Host {
        app,
        tools,
        gates,
        services,
    })
}

/// The testbed, as `name`, with `config` and the three capabilities between plugins scoped as
/// given. Everything else it imports is linked with an empty scope.
fn testbed(
    dir: &Path,
    name: &str,
    config: &Value,
    [tools, state, service]: [Scope; 3],
) -> Result<WasmPlugin, Box<dyn Error>> {
    Ok(WasmRuntime::new()?
        .with_root(dir)
        .load(plugin("testbed"))?
        .named(name)
        .config(config.to_string())
        .grant("log", Scope::none())
        .grant("clock", Scope::none())
        .grant("random", Scope::none())
        .grant("tools", tools)
        .grant("state", state)
        .grant("service", service)
        .grant("events", Scope::none())
        .grant("plugins", Scope::none()))
}

/// Call the tool `tool` with `input` and parse what it returns.
fn call(host: &Host, tool: &str, input: &Value) -> Result<Value, Box<dyn Error>> {
    Ok(serde_json::from_str(
        &host.tools.call(tool, &input.to_string())?,
    )?)
}

fn payloads(app: &App, topic: &str) -> Vec<Value> {
    app.events()
        .matching(&Pattern::new(topic))
        .iter()
        .filter_map(|event| serde_json::from_str(&event.payload).ok())
        .collect()
}

#[test]
fn a_plugin_calls_tools_within_its_scope_and_through_the_gates() {
    let dir = scratch("plugin-tools");
    let mut host = host(EventStream::new()).unwrap();
    // A gate that holds plugins, but not the model, to tools that only read.
    let gate: Arc<dyn Gate> = Arc::new(|_: &str, request: &str| {
        let request: Value = serde_json::from_str(request).ok()?;
        let from_plugin = request["caller"].as_str()?.starts_with("plugin:");
        Some(if from_plugin && request["read-only"] == false {
            Verdict::Deny("plugins may only read".into())
        } else {
            Verdict::Allow
        })
    });
    host.gates.insert("readers", gate).unwrap();
    let scoped = [
        Scope::none().allow("peek").allow("poke"),
        Scope::none(),
        Scope::none(),
    ];
    host.app
        .plugin(testbed(&dir, "testbed", &json!({}), scoped).unwrap());

    let peeked = call(&host, "relay", &json!({"tool": "peek"})).unwrap();
    let poked = call(&host, "relay", &json!({"tool": "poke"})).unwrap();
    let outside = call(&host, "relay", &json!({"tool": "count"})).unwrap();

    assert_eq!(peeked, json!({"ok": "peek"}));
    assert_eq!(
        poked["error"],
        r#"ToolError::Denied("denied by gate `readers`: plugins may only read")"#
    );
    assert_eq!(
        outside["error"],
        r#"ToolError::Denied("capability `tools` does not cover `count`")"#
    );
    // Each call the plugin made is on the stream, with the plugin as its source and caller.
    let calls = host.app.events().matching(&Pattern::new("tool/call"));
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].source.to_string(), "plugin:testbed");
    let decisions = payloads(&host.app, "gate/decision");
    assert_eq!(decisions[1]["verdict"], "deny");
    assert_eq!(decisions[1]["caller"], "plugin:testbed");
    let results = payloads(&host.app, "tool/result");
    assert_eq!(results[0]["output"], "peek");
    assert_eq!(results[1]["ok"], false);
}

#[test]
fn a_gate_that_asks_cannot_hold_a_plugins_call() {
    let dir = scratch("plugin-tools-ask");
    let mut host = host(EventStream::new()).unwrap();
    let ask: Arc<dyn Gate> = Arc::new(|_: &str, _: &str| Some(Verdict::Ask("sure?".into())));
    host.gates.insert("asker", ask).unwrap();
    let scoped = [Scope::any(), Scope::none(), Scope::none()];
    host.app
        .plugin(testbed(&dir, "testbed", &json!({}), scoped).unwrap());

    let asked = call(&host, "relay", &json!({"tool": "peek"})).unwrap();

    assert_eq!(
        asked["error"],
        "ToolError::Denied(\"gate `asker` asked for a person's approval, which a plugin's call cannot \
         wait for: sure?\")"
    );
}

#[test]
fn a_plugin_that_gates_tool_calls_may_not_make_them() {
    let dir = scratch("gate-and-tools");
    let mut host = host(EventStream::new()).unwrap();
    let scoped = [Scope::any(), Scope::none(), Scope::none()];

    let id = host
        .app
        .plugin(testbed(&dir, "testbed", &json!({"gates": ["tool/call"]}), scoped).unwrap());

    let Some(FiberState::Failed(reason)) = host.app.state(id) else {
        panic!("a gate that imports `tools` must not start");
    };
    assert_eq!(
        reason,
        "plugin `testbed` gates `tool/call` and imports `tools`: a plugin may decide such \
         calls or make them, not both"
    );
}

#[test]
fn a_plugin_is_never_called_back_while_it_runs() {
    let dir = scratch("reentry");
    let mut host = host(EventStream::new()).unwrap();
    let scoped = [Scope::any(), Scope::none(), Scope::none().allow("testbed")];
    let config = json!({"provides": ["testbed"]});
    host.app
        .plugin(testbed(&dir, "testbed", &config, scoped).unwrap());

    // Its own tool, and its own service, while it is running the call that asks for them.
    let tool = call(&host, "relay", &json!({"tool": "count"})).unwrap();
    let service = call(
        &host,
        "relay",
        &json!({"service": "testbed", "method": "echo", "payload": "hi"}),
    )
    .unwrap();

    let refused = "plugin `testbed` is already running further up this call: a plugin is never \
                   called back while it runs";
    assert_eq!(tool["error"], format!("ToolError::Failed({refused:?})"));
    assert_eq!(service["error"], format!("CallError::Failed({refused:?})"));
    // Refusing is not a trap: the instance is the same one, its count where it was.
    assert_eq!(host.tools.call("count", "{}").unwrap(), "1");
    assert_eq!(payloads(&host.app, "plugin/trapped"), Vec::<Value>::new());
}

/// Two testbeds: `alpha`, which may call `beta`, and `beta`, which may call `alpha` and
/// `native`. Each provides a service under its own name.
fn pair(host: &mut Host, dir: &Path) -> Result<(), Box<dyn Error>> {
    host.app.plugin(testbed(
        dir,
        "alpha",
        &json!({"provides": ["alpha"], "prefix": "a."}),
        [Scope::none(), Scope::none(), Scope::none().allow("beta")],
    )?);
    host.app.plugin(testbed(
        dir,
        "beta",
        &json!({"provides": ["beta"], "prefix": "b."}),
        [
            Scope::none(),
            Scope::none(),
            Scope::none().allow("alpha").allow("native"),
        ],
    )?);
    Ok(())
}

/// Have `beta` call `method` of `service` with `payload`, and say what came of it.
fn via(host: &Host, service: &str, method: &str, payload: &Value) -> Value {
    let input = json!({"service": service, "method": method, "payload": payload.to_string()});
    call(host, "b.relay", &input).unwrap_or_else(|error| json!({"host": error.to_string()}))
}

#[test]
fn a_cycle_of_service_calls_is_refused_where_it_closes() {
    let dir = scratch("service-cycle");
    let mut host = host(EventStream::new()).unwrap();
    pair(&mut host, &dir).unwrap();

    // Beta asks alpha to call beta back: beta is still running, so alpha is refused.
    let cycle = via(
        &host,
        "alpha",
        "relay",
        &json!({"service": "beta", "method": "echo", "payload": "back"}),
    );

    let inner: Value = serde_json::from_str(cycle["ok"].as_str().unwrap()).unwrap();
    assert!(
        inner["error"]
            .as_str()
            .unwrap()
            .contains("plugin `beta` is already running further up this call"),
        "{inner}"
    );
}

#[test]
fn services_cross_between_plugins_with_the_callers_name() {
    let dir = scratch("services");
    let mut host = host(EventStream::new()).unwrap();
    let native: Arc<dyn Service> =
        Arc::new(|caller: &str, method: &str, payload: &str| match method {
            "shout" => Ok(format!("{caller} said {}", payload.to_uppercase())),
            _ => Err(CallError::Unknown(format!("method `{method}`"))),
        });
    host.services.insert("native", native).unwrap();
    pair(&mut host, &dir).unwrap();
    let via_beta =
        |service: &str, method: &str, payload: &Value| via(&host, service, method, payload);

    let echoed = via_beta("alpha", "echo", &json!("hello"));
    let shouted = via_beta("native", "shout", &json!("hello"));
    let unknown = via_beta("alpha", "dance", &json!(null));
    let outside = via_beta("gamma", "echo", &json!(null));

    let echoed: Value = serde_json::from_str(echoed["ok"].as_str().unwrap()).unwrap();
    assert_eq!(
        echoed,
        json!({"caller": "beta", "service": "alpha", "payload": "\"hello\""})
    );
    assert_eq!(shouted, json!({"ok": "beta said \"HELLO\""}));
    assert_eq!(unknown["error"], r#"CallError::Unknown("method `dance`")"#);
    assert_eq!(
        outside["error"],
        r#"CallError::Denied("capability `service` does not cover `gamma`")"#
    );
    // A native caller reaches a WebAssembly service the same way.
    assert_eq!(
        host.services.call("host", "alpha", "echo", "x").unwrap(),
        r#"{"caller":"host","payload":"x","service":"alpha"}"#
    );
}

#[test]
fn state_is_kept_across_restarts_reloads_and_processes_but_not_from_a_trapped_call() {
    let dir = scratch("state");
    let mut host = host(EventStream::new()).unwrap();
    let scoped = || {
        [
            Scope::none(),
            Scope::none().allow("public/*"),
            Scope::none(),
        ]
    };
    let id = host
        .app
        .plugin(testbed(&dir, "testbed", &json!({}), scoped()).unwrap());
    let state = |host: &Host, input: Value| call(host, "state", &input).unwrap();

    assert_eq!(
        state(&host, json!({"op": "set", "key": "public/a", "value": "1"})),
        true
    );
    assert_eq!(
        state(&host, json!({"op": "set", "key": "secret", "value": "x"})),
        json!({"denied": "capability `state` does not cover `secret`"})
    );
    // This write is made and then the call traps, so it is not kept.
    host.tools
        .call(
            "state",
            &json!({"op": "set", "key": "public/b", "value": "2", "trap": true}).to_string(),
        )
        .unwrap_err();
    // The next call restarts the plugin, which still has what its returned calls wrote.
    assert_eq!(state(&host, json!({"op": "get", "key": "public/a"})), "1");
    assert_eq!(state(&host, json!({"op": "keys"})), json!(["public/a"]));

    host.app.reload(id).unwrap();
    assert_eq!(state(&host, json!({"op": "get", "key": "public/a"})), "1");

    // A new process: the same events, a new app, a new plugin.
    let events = EventStream::replay(host.app.events().events());
    let mut next = self::host(events).unwrap();
    next.app
        .plugin(testbed(&dir, "testbed", &json!({}), scoped()).unwrap());
    assert_eq!(state(&next, json!({"op": "get", "key": "public/a"})), "1");
    assert_eq!(
        state(&next, json!({"op": "remove", "key": "public/a"})),
        true
    );
    assert_eq!(state(&next, json!({"op": "keys"})), json!([]));
    assert_eq!(
        payloads(&next.app, "state/*").last(),
        Some(&json!({"key": "public/a"}))
    );
}

/// A tool that keeps its caller busy, and the two ends that control it.
struct Hold {
    tool: Arc<dyn Tool>,
    /// Hears when the tool is called.
    entered: mpsc::Receiver<()>,
    /// Lets the tool return (it gives up by itself after ten seconds).
    release: mpsc::Sender<()>,
}

fn hold() -> Hold {
    let (entered, on_entered) = mpsc::channel();
    let (release, on_release) = mpsc::channel::<()>();
    let (entered, on_release) = (Mutex::new(entered), Mutex::new(on_release));
    let spec = ToolSpec {
        name: "hold".into(),
        description: String::new(),
        input_schema: r#"{"type":"object"}"#.into(),
        replay_safe: true,
        read_only: true,
    };
    let tool = tool_fn(spec, move |_| {
        if let Ok(entered) = entered.lock() {
            entered.send(()).unwrap_or_default();
        }
        if let Ok(on_release) = on_release.lock() {
            on_release
                .recv_timeout(Duration::from_secs(10))
                .unwrap_or_default();
        }
        Ok("held".to_owned())
    });
    Hold {
        tool: Arc::new(tool),
        entered: on_entered,
        release,
    }
}

/// The testbed as `alpha`, providing the service `alpha`, allowed to call `hold`, and making
/// calls from inside other plugins wait at most 200 ms for it.
fn impatient_alpha() -> Result<WasmPlugin, Box<dyn Error>> {
    let limits = Limits {
        nested_wait: Duration::from_millis(200),
        ..Limits::default()
    };
    Ok(WasmRuntime::new()?
        .with_limits(limits)
        .load(plugin("testbed"))?
        .named("alpha")
        .config(json!({"provides": ["alpha"], "prefix": "a."}).to_string())
        .grant("log", Scope::none())
        .grant("clock", Scope::none())
        .grant("random", Scope::none())
        .grant("tools", Scope::none().allow("hold"))
        .grant("state", Scope::none())
        .grant("service", Scope::none())
        .grant("events", Scope::none())
        .grant("plugins", Scope::none()))
}

#[test]
fn a_call_from_inside_a_plugin_waits_only_so_long_for_one_busy_elsewhere() {
    let dir = scratch("nested-wait");
    let mut host = host(EventStream::new()).unwrap();
    let hold = hold();
    host.tools.insert("hold", hold.tool).unwrap();
    host.app.plugin(impatient_alpha().unwrap());
    let beta = testbed(
        &dir,
        "beta",
        &json!({"prefix": "b."}),
        [Scope::none(), Scope::none(), Scope::none().allow("alpha")],
    );
    host.app.plugin(beta.unwrap());
    let tools = Arc::clone(&host.tools);

    // Alpha is busy on another thread, inside `hold`.
    let busy = thread::spawn(move || tools.call("a.relay", r#"{"tool":"hold"}"#));
    hold.entered.recv_timeout(Duration::from_secs(10)).unwrap();
    let started = Instant::now();
    let refused = via(&host, "alpha", "echo", &json!("hi"));
    let waited = started.elapsed();
    hold.release.send(()).unwrap();

    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("plugin `alpha` stayed busy with another call for 200ms"),
        "{refused}"
    );
    assert!(waited >= Duration::from_millis(200), "{waited:?}");
    assert_eq!(busy.join().unwrap().unwrap(), r#"{"ok":"held"}"#);
    // Once alpha is free, the same call goes through.
    assert!(via(&host, "alpha", "echo", &json!("hi"))["ok"].is_string());
}
