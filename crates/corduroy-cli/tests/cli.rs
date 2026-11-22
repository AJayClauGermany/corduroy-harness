//! The `corduroy` binary end to end: an agent file, a model behind a fake OpenAI-compatible
//! endpoint on the loopback interface, the Pi tools and the approval gate, and a person
//! answering at the terminal. No network and no real model.

use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use corduroy_test_support::{plugin, scratch};
use serde_json::{Value, json};

/// The body of one HTTP request.
fn read_body(stream: &TcpStream) -> String {
    let mut reader = BufReader::new(stream);
    let mut length = 0;
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
        line.clear();
    }
    let mut body = vec![0; length];
    let _read = reader.read_exact(&mut body);
    String::from_utf8_lossy(&body).into_owned()
}

/// The requests a fake endpoint received, in order.
type Seen = Arc<Mutex<Vec<Value>>>;

/// A chat-completions endpoint that answers with `replies` in turn, and keeps each request.
fn model(replies: Vec<Value>) -> std::io::Result<(SocketAddr, Seen)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&seen);
    thread::spawn(move || {
        for (stream, reply) in listener.incoming().flatten().zip(replies) {
            let request = serde_json::from_str(&read_body(&stream)).unwrap_or(Value::Null);
            if let Ok(mut seen) = kept.lock() {
                seen.push(request);
            }
            let body = reply.to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
            let _written = (&stream).write_all(response.as_bytes());
        }
    });
    Ok((address, seen))
}

fn completion(message: &Value, finish: &str) -> Value {
    json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "created": 0,
        "model": "fake",
        "choices": [{"index": 0, "message": message, "finish_reason": finish}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
    })
}

fn tool_call(name: &str, arguments: &Value) -> Value {
    completion(
        &json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": name, "arguments": arguments.to_string()},
            }],
        }),
        "tool_calls",
    )
}

fn answer(text: &str) -> Value {
    completion(&json!({"role": "assistant", "content": text}), "stop")
}

/// An agent file using the fake model at `address`, with the Pi tools confined to the
/// workspace and the approval gate in `smart` mode.
fn agent_file(address: SocketAddr, plugins: &Path) -> String {
    format!(
        r#"
provider = "fake"
model = "fake-model"

[providers.fake]
kind = "openai-compatible"
base_url = "http://{address}/v1/chat/completions"
streaming = false

[kernels.shell]
kind = "shell"

[[plugins]]
name = "pi-tools"
path = "{tools}"
config = {{ kernel = "shell" }}
grants = {{ fs-read = ["{{workspace}}", "{{workspace}}/*"], fs-write = ["{{workspace}}", "{{workspace}}/*"], kernel = "shell" }}

[[plugins]]
name = "approval"
path = "{approval}"
config = {{ mode = "smart" }}
"#,
        tools = plugins.join("pi_tools.wasm").display(),
        approval = plugins.join("approval.wasm").display(),
    )
}

/// Run `corduroy` in `workspace` with `args`, typing `typed` at it.
fn corduroy(workspace: &Path, args: &[&str], typed: &str) -> std::io::Result<Output> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_corduroy"))
        .arg("--workspace")
        .arg(workspace)
        .args(args)
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(typed.as_bytes())?;
    }
    child.wait_with_output()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn a_prompt_runs_through_a_real_provider_tools_and_a_person_answering_the_gate() {
    let workspace = scratch("cli-run");
    let plugins = plugin("pi-tools").parent().unwrap().to_owned();
    let target = workspace.join("hello.txt");
    let (address, seen) = model(vec![
        tool_call("write", &json!({"path": target, "content": "hi\n"})),
        answer("I wrote hello.txt."),
    ])
    .unwrap();
    fs::write(
        workspace.join("corduroy.toml"),
        agent_file(address, &plugins),
    )
    .unwrap();

    // `y` answers the approval gate, which asks before a tool that writes.
    let output = corduroy(&workspace, &["run", "say", "hi", "in", "a", "file"], "y\n").unwrap();
    let stdout = text(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        text(&output.stderr)
    );
    assert!(stdout.contains("▸ write"), "{stdout}");
    assert!(stdout.contains("approval asks"), "{stdout}");
    assert!(stdout.contains("✓ Wrote 3 bytes"), "{stdout}");
    assert!(stdout.contains("I wrote hello.txt."), "{stdout}");
    assert_eq!(fs::read_to_string(&target).unwrap(), "hi\n");

    // The model saw the tools and the user's prompt; the key is in no request.
    let seen = seen.lock().unwrap().clone();
    let tools: Vec<&str> = seen[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str())
        .collect();
    assert!(
        tools.contains(&"bash") && tools.contains(&"write"),
        "{tools:?}"
    );
    assert!(seen[0].to_string().contains("say hi in a file"));

    // Everything is in the log, which a later process reads back.
    let log = corduroy(&workspace, &["log"], "").unwrap();
    let log = text(&log.stdout);
    for topic in [
        "tool/call",
        "gate/decision",
        "gate/answer",
        "tool/result",
        "turn/end",
    ] {
        assert!(log.contains(topic), "{topic} missing from\n{log}");
    }
}

/// With nobody at the terminal the run waits in the log; a later process answers it.
#[test]
fn a_run_waits_across_processes_for_its_answer() {
    let workspace = scratch("cli-wait");
    let plugins = plugin("pi-tools").parent().unwrap().to_owned();
    let target = workspace.join("later.txt");
    let (address, _seen) = model(vec![
        tool_call("write", &json!({"path": target, "content": "later"})),
        answer("Done, once you said so."),
    ])
    .unwrap();
    fs::write(
        workspace.join("corduroy.toml"),
        agent_file(address, &plugins),
    )
    .unwrap();

    let first = corduroy(&workspace, &["run", "write", "later"], "").unwrap();
    assert!(!first.status.success());
    assert!(text(&first.stdout).contains("corduroy continue"));
    assert!(!target.exists());

    let second = corduroy(&workspace, &["continue"], "y\n").unwrap();
    let stdout = text(&second.stdout);
    assert!(
        second.status.success(),
        "{stdout}\n{}",
        text(&second.stderr)
    );
    assert!(stdout.contains("Done, once you said so."), "{stdout}");
    assert_eq!(fs::read_to_string(&target).unwrap(), "later");
}

#[test]
fn a_declined_call_goes_back_to_the_model_with_the_reason() {
    let workspace = scratch("cli-decline");
    let plugins = plugin("pi-tools").parent().unwrap().to_owned();
    let target = workspace.join("nope.txt");
    let (address, seen) = model(vec![
        tool_call("write", &json!({"path": target, "content": "x"})),
        answer("Understood, I left it alone."),
    ])
    .unwrap();
    fs::write(
        workspace.join("corduroy.toml"),
        agent_file(address, &plugins),
    )
    .unwrap();

    let output = corduroy(&workspace, &["run", "write", "nope"], "not today\n").unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(!target.exists());
    // The second request carries the person's reason as the tool's error.
    let seen = seen.lock().unwrap().clone();
    assert!(seen[1].to_string().contains("not today"), "{}", seen[1]);
}

#[test]
fn check_says_what_the_agent_has_and_a_plugin_cannot_read_the_agent_file() {
    let workspace = scratch("cli-check");
    let plugins = plugin("pi-tools").parent().unwrap().to_owned();
    let agent = workspace.join("corduroy.toml");
    let (address, _seen) = model(vec![
        tool_call("read", &json!({"path": agent})),
        answer("I could not read it."),
    ])
    .unwrap();
    fs::write(&agent, agent_file(address, &plugins)).unwrap();

    let check = corduroy(&workspace, &["check"], "").unwrap();
    let report = text(&check.stdout);
    assert!(check.status.success(), "{report}\n{}", text(&check.stderr));
    assert!(report.contains("tool      read (read-only)"), "{report}");
    assert!(report.contains("gates     approval"), "{report}");

    // `read` only reads, so smart mode lets it run; the host still refuses the agent file.
    let output = corduroy(&workspace, &["run", "read", "your", "grants"], "").unwrap();
    let stdout = text(&output.stdout);
    assert!(
        stdout.contains("! pi-tools was refused `fs-read`"),
        "{stdout}"
    );
}

#[test]
fn a_broken_agent_file_is_reported_with_where() {
    let workspace = scratch("cli-broken");
    fs::write(
        workspace.join("corduroy.toml"),
        "provider = \"x\"\nmodel = 3\n",
    )
    .unwrap();
    let output = corduroy(&workspace, &["check"], "").unwrap();
    assert!(!output.status.success());
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("corduroy.toml") && stderr.contains("model"),
        "{stderr}"
    );
}

/// An MCP server in the agent file: started through the shell kernel, its tools offered to
/// the model under the server's name. The server is a Python script, so this needs `python3`.
#[test]
fn an_mcp_server_in_the_agent_file_gives_the_model_its_tools() {
    let workspace = scratch("cli-mcp");
    let plugins = plugin("pi-tools").parent().unwrap().to_owned();
    let server =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../corduroy-mcp/tests/fixtures/server.py");
    let (address, seen) = model(vec![
        tool_call("fixture__add", &json!({"a": 20, "b": 22})),
        answer("It is 42."),
    ])
    .unwrap();
    let agent = format!(
        "{}\n[mcp.fixture]\ncommand = \"python3\"\nargs = [\"{}\"]\n",
        agent_file(address, &plugins),
        server.display()
    );
    fs::write(workspace.join("corduroy.toml"), agent).unwrap();

    let output = corduroy(&workspace, &["run", "add", "them"], "y\n").unwrap();
    let stdout = text(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        text(&output.stderr)
    );
    assert!(stdout.contains("▸ fixture__add"), "{stdout}");
    assert!(stdout.contains("✓ 42"), "{stdout}");
    let seen = seen.lock().unwrap().clone();
    assert!(seen[0].to_string().contains("fixture__echo"));
}
