//! An MCP server over standard input and output, started through a shell kernel: its tools
//! in the registry, called, and gone with the plugin. The server is a small Python script in
//! `tests/fixtures`, so these tests need `python3`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use corduroy_core::{App, FiberState, KERNELS, Kernels, Spawn, TOOLS, ToolError, Tools};
use corduroy_mcp::{Transport, mcp_server};
use corduroy_shell::shell_kernel;

struct Host {
    app: App,
    tools: Arc<Tools>,
}

fn host(dir: &Path) -> Result<Host, Box<dyn std::error::Error>> {
    let tools = Arc::new(Tools::new());
    let mut app = App::new();
    app.provide(TOOLS, Arc::clone(&tools))?;
    app.provide(KERNELS, Arc::new(Kernels::new()))?;
    app.plugin(shell_kernel("shell", dir));
    Ok(Host { app, tools })
}

fn fixture() -> Transport {
    let server = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/server.py");
    Transport::Stdio {
        kernel: "shell".into(),
        spawn: Spawn {
            program: "python3".into(),
            args: vec![server.to_string_lossy().into_owned()],
            env: vec![("GREETING".into(), "hello".into())],
        },
    }
}

#[test]
fn a_servers_tools_are_offered_under_its_name_and_called() {
    let dir = std::env::temp_dir().canonicalize().unwrap();
    let mut host = host(&dir).unwrap();
    let id = host
        .app
        .plugin(mcp_server("fixture", fixture()).with_timeout(Duration::from_secs(10)));
    assert_eq!(host.app.state(id), Some(&FiberState::Active));

    let specs = host.tools.specs();
    let flags: Vec<(&str, bool, bool)> = specs
        .iter()
        .map(|spec| (spec.name.as_str(), spec.read_only, spec.replay_safe))
        .collect();
    assert_eq!(
        flags,
        [
            ("fixture__add", false, true),
            ("fixture__echo", true, true),
            ("fixture__fail", false, false),
            ("fixture__where", false, false),
        ]
    );
    assert!(specs[0].input_schema.contains("\"a\""));

    assert_eq!(
        host.tools
            .call("fixture__echo", r#"{"text":"hi"}"#)
            .unwrap(),
        "hi"
    );
    assert_eq!(
        host.tools.call("fixture__add", r#"{"a":2,"b":3}"#).unwrap(),
        "5"
    );
    assert_eq!(
        host.tools.call("fixture__fail", "{}"),
        Err(ToolError::Failed("boom".into()))
    );
    assert!(matches!(
        host.tools.call("fixture__echo", "[1]"),
        Err(ToolError::InvalidInput(_))
    ));
    // Started by the kernel: in its directory, with the environment asked for.
    let place = host.tools.call("fixture__where", "{}").unwrap();
    assert_eq!(place, format!("{} hello", dir.display()));

    host.app.dispose(id).unwrap();
    assert_eq!(host.tools.specs(), []);
}

#[test]
fn a_server_that_does_not_start_fails_the_plugin() {
    let dir = std::env::temp_dir();
    let mut host = host(&dir).unwrap();
    let missing = Transport::Stdio {
        kernel: "shell".into(),
        spawn: Spawn {
            program: "/no/such/server".into(),
            ..Spawn::default()
        },
    };
    let id = host.app.plugin(mcp_server("missing", missing));
    assert!(
        matches!(host.app.state(id), Some(FiberState::Failed(reason)) if reason.contains("/no/such/server"))
    );
    let silent = Transport::Stdio {
        kernel: "shell".into(),
        spawn: Spawn {
            program: "sleep".into(),
            args: vec!["30".into()],
            env: Vec::new(),
        },
    };
    let id = host
        .app
        .plugin(mcp_server("silent", silent).with_timeout(Duration::from_millis(500)));
    assert!(
        matches!(host.app.state(id), Some(FiberState::Failed(reason)) if reason.contains("did not start")),
        "{:?}",
        host.app.state(id)
    );
}
