//! WebAssembly plugins under different capability grants, through the public API only.

use std::error::Error;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use corduroy_core::{
    App, FiberState, GATES, Gates, KERNELS, Kernels, PROMPTS, Pattern, Prompts, Scope, ShellKernel,
    TOOL_CALL, TOOLS, ToolError, Tools, Verdict,
};
use corduroy_test_support::{plugin, scratch};
use corduroy_wasm::{Limits, WasmError, WasmPlugin, WasmRuntime};

struct Host {
    app: App,
    tools: Arc<Tools>,
    prompts: Arc<Prompts>,
    gates: Arc<Gates>,
}

/// An app providing the services WebAssembly plugins inject, with a shell kernel in `dir`.
fn host(dir: &Path) -> Result<Host, Box<dyn Error>> {
    let tools = Arc::new(Tools::new());
    let prompts = Arc::new(Prompts::new());
    let kernels = Arc::new(Kernels::new());
    let gates = Arc::new(Gates::new());
    kernels.insert(
        "shell",
        Arc::new(Mutex::new(ShellKernel::new().in_dir(dir))),
    )?;
    let mut app = App::new();
    app.provide(TOOLS, Arc::clone(&tools))?;
    app.provide(PROMPTS, Arc::clone(&prompts))?;
    app.provide(GATES, Arc::clone(&gates))?;
    app.provide(KERNELS, kernels)?;
    Ok(Host {
        app,
        tools,
        prompts,
        gates,
    })
}

fn load(name: &str, dir: &Path) -> Result<WasmPlugin, WasmError> {
    WasmRuntime::new()?.with_root(dir).load(plugin(name))
}

fn topics(app: &App, pattern: &str) -> Vec<String> {
    app.events()
        .matching(&Pattern::new(pattern))
        .into_iter()
        .map(|event| event.payload)
        .collect()
}

#[test]
fn a_plugin_declares_its_capabilities_through_its_imports() {
    let dir = scratch("imports");

    assert_eq!(
        load("pi-tools", &dir).unwrap().requires(),
        ["fs-read", "fs-write", "kernel"]
    );
}

#[test]
fn a_plugin_without_its_capabilities_is_never_instantiated() {
    let dir = scratch("ungranted");
    let mut host = host(&dir).unwrap();

    let id = host.app.plugin(
        load("pi-tools", &dir)
            .unwrap()
            .grant("fs-read", Scope::any()),
    );

    assert_eq!(
        host.app.state(id),
        Some(&FiberState::Failed(
            "plugin `pi_tools` was not granted: fs-write, kernel".into()
        ))
    );
    assert_eq!(host.tools.names(), Vec::<String>::new());
}

#[test]
fn granted_tools_work_within_their_scope() {
    let dir = scratch("granted");
    let mut host = host(&dir).unwrap();
    let scope = Scope::subtree(&dir.to_string_lossy());
    host.app.plugin(
        load("pi-tools", &dir)
            .unwrap()
            .named("pi-tools")
            .grant("fs-read", scope.clone())
            .grant("fs-write", scope)
            .grant("kernel", Scope::none().allow("shell")),
    );

    assert_eq!(host.tools.names(), ["bash", "edit", "read", "write"]);
    assert_eq!(
        host.tools.call(
            "write",
            r#"{"path":"notes/a.txt","content":"one\ntwo\nthree\n"}"#
        ),
        Ok("Wrote 14 bytes to notes/a.txt".to_owned())
    );
    assert_eq!(
        host.tools.call(
            "edit",
            r#"{"path":"notes/a.txt","edits":[{"oldText":"two","newText":"2"}]}"#
        ),
        Ok("Applied 1 edit(s) to notes/a.txt".to_owned())
    );
    assert_eq!(
        host.tools
            .call("read", r#"{"path":"notes/a.txt","offset":2,"limit":1}"#),
        Ok("2\n\n[Showing lines 2-2 of 3. Use offset=3 to continue.]".to_owned())
    );
    assert_eq!(
        fs::read_to_string(dir.join("notes/a.txt")).unwrap(),
        "one\n2\nthree\n"
    );
}

#[test]
fn bash_runs_in_a_kernel_that_keeps_its_state() {
    let dir = scratch("kernel");
    let mut host = host(&dir).unwrap();
    host.app.plugin(
        load("pi-tools", &dir)
            .unwrap()
            .grant("fs-read", Scope::none())
            .grant("fs-write", Scope::none())
            .grant("kernel", Scope::none().allow("shell")),
    );

    host.tools
        .call("bash", r#"{"command":"mkdir sub && cd sub && answer=42"}"#)
        .unwrap();

    assert_eq!(
        host.tools.call(
            "bash",
            r#"{"command":"echo $answer in $(basename $(pwd))"}"#
        ),
        Ok("42 in sub\n".to_owned())
    );
    assert_eq!(
        host.tools.call("bash", r#"{"command":"echo oops; false"}"#),
        Ok("oops\n[exit status 1]".to_owned())
    );
}

#[test]
fn calls_outside_the_scope_are_denied_and_logged() {
    let dir = scratch("scope");
    let secret = scratch("scope-secret").join("key.txt");
    fs::write(&secret, "hunter2").unwrap();
    let mut host = host(&dir).unwrap();
    host.app.plugin(
        load("pi-tools", &dir)
            .unwrap()
            .named("pi-tools")
            .grant("fs-read", Scope::subtree(&dir.to_string_lossy()))
            .grant("fs-write", Scope::none())
            .grant("kernel", Scope::none().allow("python")),
    );

    let read = host
        .tools
        .call("read", &format!(r#"{{"path":"{}"}}"#, secret.display()));
    let escape = host.tools.call("read", r#"{"path":"../../etc/passwd"}"#);
    let write = host
        .tools
        .call("write", r#"{"path":"a.txt","content":"x"}"#);
    let bash = host.tools.call("bash", r#"{"command":"cat /etc/passwd"}"#);

    for result in [&read, &escape, &write, &bash] {
        assert!(matches!(result, Err(ToolError::Denied(_))), "{result:?}");
    }
    assert!(!dir.join("a.txt").exists());
    let denied = topics(&host.app, "capability/denied");
    assert_eq!(denied.len(), 4);
    assert_eq!(
        denied[3],
        r#"{"plugin":"pi-tools","capability":"kernel","resource":"shell"}"#
    );
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_does_not_widen_a_scope() {
    let dir = scratch("symlink");
    let outside = scratch("symlink-outside");
    fs::write(outside.join("key.txt"), "hunter2").unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("link")).unwrap();
    let mut host = host(&dir).unwrap();
    host.app.plugin(
        load("pi-tools", &dir)
            .unwrap()
            .grant("fs-read", Scope::subtree(&dir.to_string_lossy()))
            .grant("fs-write", Scope::none())
            .grant("kernel", Scope::none()),
    );

    let result = host.tools.call("read", r#"{"path":"link/key.txt"}"#);

    assert!(matches!(result, Err(ToolError::Denied(_))), "{result:?}");
}

#[test]
fn unloading_a_plugin_removes_everything_it_registered() {
    let dir = scratch("unload");
    let mut host = host(&dir).unwrap();
    let skills = load("skills", &dir)
        .unwrap()
        .grant("fs-read", Scope::subtree(&dir.to_string_lossy()))
        .grant("fs-write", Scope::subtree(&dir.to_string_lossy()))
        .grant("events", Scope::none().allow("skills/*"));

    let id = host.app.plugin(skills);
    assert_eq!(
        host.tools.names(),
        ["skill_manage", "skill_view", "skills_list"]
    );
    assert_eq!(host.prompts.names(), ["skills"]);

    host.app.dispose(id).unwrap();

    assert_eq!(host.tools.names(), Vec::<String>::new());
    assert_eq!(host.prompts.names(), Vec::<String>::new());
}

#[test]
fn a_gate_answers_at_its_point_and_leaves_with_its_plugin() {
    let dir = scratch("gate");
    let mut host = host(&dir).unwrap();
    let approval = load("approval", &dir)
        .unwrap()
        .config(r#"{"mode":"smart","deny":["bash"]}"#);
    // A gate holds no authority of its own.
    assert!(approval.requires().is_empty(), "{:?}", approval.requires());

    let id = host.app.plugin(approval);
    let verdict = |tool: &str, read_only: bool| {
        let request = format!(r#"{{"tool":"{tool}","input":{{}},"read-only":{read_only}}}"#);
        host.gates
            .decide(TOOL_CALL, &request)
            .map(|decision| decision.verdict)
    };

    assert_eq!(host.gates.names(), ["approval"]);
    assert_eq!(verdict("read", true), Some(Verdict::Allow));
    assert_eq!(
        verdict("write", false),
        Some(Verdict::Ask("`write` can change things".into()))
    );
    assert_eq!(
        verdict("bash", false),
        Some(Verdict::Deny("`bash` is on the deny list".into()))
    );
    assert_eq!(host.gates.decide("model/request", "{}"), None);

    host.app.dispose(id).unwrap();
    assert_eq!(host.gates.names(), Vec::<String>::new());
}

#[test]
fn a_gate_that_cannot_be_configured_does_not_start() {
    let dir = scratch("gate-config");
    let mut host = host(&dir).unwrap();
    let approval = load("approval", &dir).unwrap().config(r#"{"mode":"yolo"}"#);

    let id = host.app.plugin(approval);

    assert!(matches!(host.app.state(id), Some(FiberState::Failed(_))));
    assert_eq!(host.gates.names(), Vec::<String>::new());
}

#[test]
fn a_runaway_plugin_runs_out_of_fuel() {
    let dir = scratch("fuel");
    let mut host = host(&dir).unwrap();
    let starved = WasmRuntime::new()
        .unwrap()
        .with_limits(Limits {
            fuel_per_call: 1_000,
            ..Limits::default()
        })
        .load(plugin("skills"))
        .unwrap()
        .grant("fs-read", Scope::any())
        .grant("fs-write", Scope::any())
        .grant("events", Scope::any());

    let id = host.app.plugin(starved);

    let Some(FiberState::Failed(reason)) = host.app.state(id) else {
        panic!("the plugin should have failed to activate");
    };
    assert!(reason.contains("fuel"), "{reason}");
    assert_eq!(topics(&host.app, "plugin/trapped").len(), 1);
}

#[test]
fn bytes_that_are_not_a_plugin_are_rejected() {
    let runtime = WasmRuntime::new().unwrap();

    assert!(runtime.load_bytes("junk", b"not wasm").is_err());
    assert!(
        runtime
            .load("/no/such/plugin.wasm")
            .unwrap_err()
            .to_string()
            .starts_with("cannot read `/no/such/plugin.wasm`")
    );
}
