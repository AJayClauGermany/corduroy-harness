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
    corduroy_with(workspace, args, typed, &[])
}

/// The same, with `env` added to the command's environment.
fn corduroy_with(
    workspace: &Path,
    args: &[&str],
    typed: &str,
    env: &[(&str, &str)],
) -> std::io::Result<Output> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_corduroy"))
        .arg("--workspace")
        .arg(workspace)
        .args(args)
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .envs(env.iter().copied())
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

#[test]
fn a_report_says_what_a_run_waited_on_and_a_session_has_its_part_of_the_log() {
    let workspace = scratch("cli-report");
    let plugins = plugin("pi-tools").parent().unwrap().to_owned();
    let target = workspace.join("hello.txt");
    let (address, _seen) = model(vec![
        tool_call("write", &json!({"path": target, "content": "hi\n"})),
        answer("I wrote hello.txt."),
    ])
    .unwrap();
    fs::write(
        workspace.join("corduroy.toml"),
        agent_file(address, &plugins),
    )
    .unwrap();
    let run = corduroy(&workspace, &["run", "write", "it"], "y\n").unwrap();
    assert!(run.status.success(), "{}", text(&run.stderr));

    // The report says what the run waited on, and names the build and the agent's model.
    let report = corduroy(&workspace, &["report"], "").unwrap();
    let report = text(&report.stdout);
    for said in [
        "corduroy 0.",
        "model     fake-model from fake",
        "run 1 of 1 in `main`: ",
        ", finished",
        "model   fake/fake-model asked with 1 messages and",
        "answered: 10 in, 5 out, stop tool_calls",
        "person  approval asked about write (call_1) in `main`: allowed",
        "tool    write (call_1) in `main`: returned",
        "[host] model/request",
    ] {
        assert!(report.contains(said), "{said} missing from\n{report}");
    }

    // A session's part of the log is its own events, each with its time to the millisecond.
    let own = corduroy(&workspace, &["--session", "main", "log"], "").unwrap();
    let own = text(&own.stdout);
    assert!(
        own.contains("turn/end") && !own.contains("process/start"),
        "{own}"
    );
    assert!(
        own.lines()
            .all(|line| line.get(19..20) == Some(".") && line.get(23..24) == Some("Z")),
        "{own}"
    );
}

#[test]
fn the_providers_settings_reach_the_model() {
    let workspace = scratch("cli-settings");
    let plugins = plugin("pi-tools").parent().unwrap().to_owned();
    let (address, seen) = model(vec![answer("Hello.")]).unwrap();
    let agent = agent_file(address, &plugins).replace(
        "streaming = false\n",
        "streaming = false\nmax_tokens = 1234\ntemperature = 0.25\n",
    );
    fs::write(workspace.join("corduroy.toml"), agent).unwrap();

    let output = corduroy(&workspace, &["run", "hi"], "").unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));

    let seen = seen.lock().unwrap().clone();
    let request = &seen[0];
    let max_tokens = request
        .get("max_tokens")
        .or_else(|| request.get("max_completion_tokens"));
    assert_eq!(max_tokens, Some(&json!(1234)), "{request}");
    assert_eq!(request["temperature"], json!(0.25), "{request}");
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
fn a_run_left_waiting_can_be_stopped_from_another_process() {
    let workspace = scratch("cli-stop");
    let plugins = plugin("pi-tools").parent().unwrap().to_owned();
    let target = workspace.join("never.txt");
    let (address, _seen) = model(vec![
        tool_call("write", &json!({"path": target, "content": "never"})),
        answer("Starting over."),
    ])
    .unwrap();
    fs::write(
        workspace.join("corduroy.toml"),
        agent_file(address, &plugins),
    )
    .unwrap();
    let waiting = corduroy(&workspace, &["run", "write", "it"], "").unwrap();
    assert!(text(&waiting.stdout).contains("corduroy continue"));

    let stopped = corduroy(&workspace, &["stop"], "").unwrap();
    assert!(stopped.status.success(), "{}", text(&stopped.stderr));
    assert!(text(&stopped.stdout).contains("stopped the run of `main`"));
    let again = corduroy(&workspace, &["stop"], "").unwrap();
    assert!(text(&again.stdout).contains("`main` has no run to stop"));

    // The write never ran, and the session takes a new prompt.
    assert!(!target.exists());
    let next = corduroy(&workspace, &["run", "start", "over"], "").unwrap();
    assert!(
        text(&next.stdout).contains("Starting over."),
        "{}",
        text(&next.stdout)
    );
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
fn each_process_records_what_it_runs_as_before_anything_else() {
    let workspace = scratch("cli-process");
    let plugins = plugin("pi-tools").parent().unwrap().to_owned();
    let (address, _seen) = model(Vec::new()).unwrap();
    fs::write(
        workspace.join("corduroy.toml"),
        agent_file(address, &plugins),
    )
    .unwrap();

    let check = corduroy(&workspace, &["check"], "").unwrap();
    assert!(check.status.success(), "{}", text(&check.stderr));
    let log = text(&corduroy(&workspace, &["log"], "").unwrap().stdout);

    // `check`, then the `log` that read it back.
    let starts: Vec<&str> = log
        .lines()
        .filter(|line| line.contains("[host] process/start"))
        .collect();
    assert_eq!(starts.len(), 2, "{log}");
    assert!(
        log.starts_with(starts[0]) && starts[0].contains(" #1 "),
        "{log}"
    );
    let version = format!(r#""version":"{}""#, env!("CARGO_PKG_VERSION"));
    for field in [
        r#""program":"corduroy""#,
        r#""world":"corduroy:plugin@0.1.0""#,
        &version,
    ] {
        assert!(
            starts[0].contains(field),
            "{field} missing from {}",
            starts[0]
        );
    }
}

#[test]
fn a_session_is_removed_with_those_it_started_and_leaves_a_marker() {
    let workspace = scratch("cli-remove");
    let plugins = plugin("pi-tools").parent().unwrap().to_owned();
    let (address, _seen) = model(Vec::new()).unwrap();
    fs::write(
        workspace.join("corduroy.toml"),
        agent_file(address, &plugins),
    )
    .unwrap();
    // `side`, and `kid` started from it at the prompt.
    let chat = corduroy(&workspace, &["--session", "side"], "/session kid\n/exit\n").unwrap();
    assert!(chat.status.success(), "{}", text(&chat.stderr));

    let removed = corduroy(&workspace, &["sessions", "remove", "side"], "").unwrap();
    let missing = corduroy(&workspace, &["sessions", "remove", "nobody"], "").unwrap();

    assert!(removed.status.success(), "{}", text(&removed.stderr));
    assert!(
        text(&removed.stdout)
            .contains("removed `side` from the log, with the sessions it started: `kid`"),
        "{}",
        text(&removed.stdout)
    );
    assert!(!missing.status.success());
    assert!(
        text(&missing.stderr).contains("there is no session `nobody`"),
        "{}",
        text(&missing.stderr)
    );
    let sessions = text(&corduroy(&workspace, &["sessions"], "").unwrap().stdout);
    assert_eq!(sessions.trim(), "main");
    let log = text(&corduroy(&workspace, &["log"], "").unwrap().stdout);
    for marker in [
        r#"session/removed {"session":"kid"}"#,
        r#"session/removed {"session":"side"}"#,
    ] {
        assert!(log.contains(marker), "{marker} missing from\n{log}");
    }
    assert!(!log.contains(" side [") && !log.contains(" kid ["), "{log}");
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

/// A sub-agent from the agent file: the model delegates, the child's work is shown indented
/// under its session, and its answer comes back to the parent.
#[test]
fn a_delegated_task_is_shown_under_its_sub_agent() {
    let workspace = scratch("cli-subagent");
    let plugins = plugin("pi-tools").parent().unwrap().to_owned();
    let (address, seen) = model(vec![
        tool_call("delegate", &json!({"task": "List the workspace."})),
        tool_call("tree", &json!({})),
        answer("It is empty but for corduroy.toml."),
        answer("The sub-agent found only corduroy.toml."),
    ])
    .unwrap();
    let agent = format!(
        "{}\n[[plugins]]\nname = \"subagents\"\npath = \"{}\"\ngrants = {{ service = \"sessions\" }}\n",
        agent_file(address, &plugins),
        plugins.join("subagents.wasm").display()
    );
    fs::write(workspace.join("corduroy.toml"), agent).unwrap();

    // `y` lets the delegate call through the approval gate; the sub-agent's tree only reads.
    let output = corduroy(&workspace, &["run", "what", "is", "here?"], "y\n").unwrap();
    let stdout = text(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        text(&output.stderr)
    );
    assert!(stdout.contains("▸ delegate"), "{stdout}");
    assert!(stdout.contains("  [main/1] ▸ tree"), "{stdout}");
    assert!(
        stdout.contains("The sub-agent found only corduroy.toml."),
        "{stdout}"
    );
    // The parent's next request carried the child's answer as the delegate call's result.
    let seen = seen.lock().unwrap().clone();
    assert!(
        seen[3]
            .to_string()
            .contains("It is empty but for corduroy.toml."),
        "{}",
        seen[3]
    );
}

/// A provider the agent does not use, whose key is not set, is reported, not fatal.
#[test]
fn a_spare_provider_without_its_key_does_not_stop_the_agent() {
    let workspace = scratch("cli-spare");
    let plugins = plugin("pi-tools").parent().unwrap().to_owned();
    let (address, _seen) = model(Vec::new()).unwrap();
    let agent = format!(
        "{}\n[providers.spare]\nkind = \"anthropic\"\napi_key_env = \"CORDUROY_TEST_UNSET_KEY\"\n",
        agent_file(address, &plugins)
    );
    fs::write(workspace.join("corduroy.toml"), agent).unwrap();

    let check = corduroy(&workspace, &["check"], "").unwrap();
    let report = text(&check.stdout);
    assert!(
        report.contains("provider spare"),
        "{report}\n{}",
        text(&check.stderr)
    );
    assert!(report.contains("CORDUROY_TEST_UNSET_KEY"), "{report}");
    assert!(report.contains("tool      read"), "{report}");
}

/// The second of the six guarantees, as far as it is built: the shell holds no variable of
/// the command's own environment, where the provider's key is, unless the agent file says.
#[test]
fn the_shell_sees_the_variables_the_agent_file_lists_and_not_the_providers_key() {
    let workspace = scratch("cli-shell-env");
    let plugins = plugin("pi-tools").parent().unwrap().to_owned();
    let shown = "echo \"key=[$FAKE_MODEL_KEY] set=[$GREETING] passed=[$PASSED] not=[$NOT_PASSED]\"";
    let (address, _seen) = model(vec![
        tool_call("bash", &json!({"command": shown})),
        answer("Looked."),
    ])
    .unwrap();
    let agent = agent_file(address, &plugins)
        .replace(
            "streaming = false\n",
            "streaming = false\napi_key_env = \"FAKE_MODEL_KEY\"\n",
        )
        .replace(
            "kind = \"shell\"\n",
            "kind = \"shell\"\nenv = { GREETING = \"hello\" }\npass = [\"PASSED\"]\n",
        );
    fs::write(workspace.join("corduroy.toml"), agent).unwrap();
    let env = [
        ("FAKE_MODEL_KEY", "s3cret-key"),
        ("PASSED", "through"),
        ("NOT_PASSED", "kept"),
    ];

    // `y` answers the approval gate, which asks before a command runs.
    let output = corduroy_with(&workspace, &["run", "look"], "y\n", &env).unwrap();
    let stdout = text(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        text(&output.stderr)
    );

    let log = corduroy_with(&workspace, &["log"], "", &env).unwrap();
    let log = text(&log.stdout);
    assert!(
        log.contains("key=[] set=[hello] passed=[through] not=[]"),
        "{log}\n{stdout}"
    );
    assert!(!log.contains("s3cret-key"), "{log}");
}

#[test]
fn without_an_agent_file_no_key_is_read_and_the_agent_says_what_it_is() {
    let workspace = scratch("cli-no-agent-file");
    // Keys in the environment, which used to choose whose model is asked.
    let env = [
        ("ANTHROPIC_API_KEY", "sk-ant-unused"),
        ("OPENAI_API_KEY", "sk-unused"),
    ];

    let check = corduroy_with(&workspace, &["check"], "", &env).unwrap();
    let said = text(&check.stdout);
    assert!(check.status.success(), "{said}\n{}", text(&check.stderr));
    assert!(said.contains("model     qwen3 from main"), "{said}");
    assert!(said.contains("providers main\n"), "{said}");

    // Nothing to keep, and nothing read from the environment for a provider.
    let secrets = corduroy_with(&workspace, &["secrets"], "", &env).unwrap();
    assert_eq!(
        text(&secrets.stdout),
        "this agent names no secret and needs no key\n"
    );

    // Talking to it says which agent it is, before anything is asked of a model.
    let chat = corduroy_with(&workspace, &[], "/exit\n", &env).unwrap();
    let said = text(&chat.stdout);
    assert!(
        said.starts_with(
            "no corduroy.toml here: this is the default agent, which asks Ollama on this machine \
             for `qwen3` and reads no key. `corduroy init` writes an agent"
        ),
        "{said}"
    );
    // The keys are in nothing it wrote.
    let log = text(
        &corduroy_with(&workspace, &["log"], "", &env)
            .unwrap()
            .stdout,
    );
    assert!(!log.contains("sk-ant-unused") && !said.contains("sk-ant-unused"));
}
