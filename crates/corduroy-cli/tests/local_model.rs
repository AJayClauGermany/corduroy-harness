//! Evals: the `corduroy` binary doing small real tasks with a real model, the one in
//! `examples/local-model/corduroy.toml`, served on this machine by `scripts/local-model.sh`.
//!
//! These are not unit tests of the harness: a model chooses what happens, so each one checks
//! only the outcome (a file's contents, a test that now passes, a tool that succeeded, a
//! fact in the answer), never how the model phrases it. They are ignored by default, because
//! they need the server and take tens of seconds each; run them with
//!
//! ```sh
//! scripts/local-model.sh start
//! cargo eval
//! ```
//!
//! `CORDUROY_LOCAL_MODEL_URL` points them at another chat-completions endpoint.

use std::fs;
use std::io;
use std::net::{TcpStream, ToSocketAddrs as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use corduroy_test_support::{plugin, scratch};

/// Where the example agent expects the model.
const DEFAULT_URL: &str = "http://127.0.0.1:8080/v1/chat/completions";

/// The plugins the example agent loads.
const PLUGINS: [&str; 11] = [
    "pi-tools",
    "skills",
    "approval",
    "inspect",
    "hints",
    "todo",
    "memory",
    "recall",
    "summarize",
    "subagents",
    "compact",
];

/// The repository root.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn other(message: String) -> io::Error {
    io::Error::other(message)
}

/// The endpoint the evals use, once something is listening there.
fn endpoint() -> io::Result<String> {
    let url = std::env::var("CORDUROY_LOCAL_MODEL_URL").unwrap_or_else(|_| DEFAULT_URL.to_owned());
    let authority = url
        .strip_prefix("http://")
        .and_then(|rest| rest.split('/').next())
        .ok_or_else(|| other(format!("{url}: expected an http:// URL")))?;
    let address = authority
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| other(format!("{authority}: no address")))?;
    TcpStream::connect_timeout(&address, Duration::from_secs(2)).map_err(|error| {
        other(format!(
            "no model at {url} ({error}): start one with `scripts/local-model.sh start`"
        ))
    })?;
    Ok(url)
}

/// A fresh workspace for `test` holding `files`, and the example agent file beside it
/// (outside the workspace), pointed at the endpoint.
fn workspace(test: &str, files: &[(&str, &str)]) -> io::Result<(PathBuf, PathBuf)> {
    for name in PLUGINS {
        // Refuses a plugin that is missing or older than its sources.
        let _built = plugin(name);
    }
    let url = endpoint()?;
    let dir = scratch(test);
    let workspace = dir.join("workspace");
    fs::create_dir_all(&workspace)?;
    for (name, contents) in files {
        fs::write(workspace.join(name), contents)?;
    }
    let example = fs::read_to_string(root().join("examples/local-model/corduroy.toml"))?;
    let agent = dir.join("corduroy.toml");
    fs::write(&agent, example.replace(DEFAULT_URL, &url))?;
    Ok((workspace, agent))
}

/// Run `corduroy run prompt` as `agent` in `workspace`, in session `session`, with nobody at
/// the terminal.
fn run(workspace: &Path, agent: &Path, session: &str, prompt: &str) -> io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_corduroy"))
        .arg("--agent")
        .arg(agent)
        .arg("--workspace")
        .arg(workspace)
        .args(["--session", session, "run", prompt])
        .stdin(Stdio::null())
        .output()
}

/// What the run printed, once it has finished successfully.
fn finished(output: &Output) -> io::Result<String> {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(other(format!(
            "the run failed ({}):\n{stdout}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

/// Whether the terminal shows a call of `tool` that succeeded: its `▸` line followed, after
/// any other calls of the same step, by a `✓` result.
fn succeeded(stdout: &str, tool: &str) -> bool {
    let call = format!("▸ {tool} ");
    let mut lines = stdout.lines().skip_while(|line| !line.starts_with(&call));
    lines.next().is_some()
        && lines
            .find(|line| line.starts_with("  ✓") || line.starts_with("  ✗"))
            .is_some_and(|line| line.starts_with("  ✓"))
}

#[test]
#[ignore = "needs a local model: scripts/local-model.sh start, then cargo eval"]
fn reads_one_file_and_writes_another() {
    let (workspace, agent) =
        workspace("eval-sum", &[("fruit.txt", "apples 3\npears 5\nplums 2\n")]).unwrap();
    let output = run(
        &workspace,
        &agent,
        "sum",
        "Read fruit.txt, then write total.txt containing only the sum of the numbers in it.",
    )
    .unwrap();
    let stdout = finished(&output).unwrap();
    let total = fs::read_to_string(workspace.join("total.txt")).unwrap_or_default();
    assert_eq!(total.trim(), "10", "{stdout}");
}

#[test]
#[ignore = "needs a local model: scripts/local-model.sh start, then cargo eval"]
fn fixes_a_failing_test_and_runs_it_again() {
    let (workspace, agent) = workspace(
        "eval-fix",
        &[
            ("calc.py", "def add(a, b):\n    return a - b\n"),
            (
                "test_calc.py",
                "from calc import add\nassert add(2, 3) == 5\nprint(\"ok\")\n",
            ),
        ],
    )
    .unwrap();
    let output = run(
        &workspace,
        &agent,
        "fix",
        "`python3 test_calc.py` fails. Find the bug in calc.py, fix it, and run the test \
         again to confirm.",
    )
    .unwrap();
    let stdout = finished(&output).unwrap();
    assert!(succeeded(&stdout, "bash"), "{stdout}");
    let test = Command::new("python3")
        .arg("test_calc.py")
        .current_dir(&workspace)
        .output()
        .unwrap();
    assert!(test.status.success(), "{stdout}");
    assert_eq!(
        fs::read_to_string(workspace.join("test_calc.py")).unwrap(),
        "from calc import add\nassert add(2, 3) == 5\nprint(\"ok\")\n",
        "the test itself was changed:\n{stdout}"
    );
}

/// `summarize` asks the model itself, through the `llm` capability rather than the loop.
#[test]
#[ignore = "needs a local model: scripts/local-model.sh start, then cargo eval"]
fn a_plugin_asks_the_same_model_through_llm() {
    let (workspace, agent) = workspace(
        "eval-summarize",
        &[("calc.py", "def add(a, b):\n    return a + b\n")],
    )
    .unwrap();
    let output = run(
        &workspace,
        &agent,
        "summarize",
        "Use the summarize tool on calc.py and tell me what it says.",
    )
    .unwrap();
    let stdout = finished(&output).unwrap();
    assert!(succeeded(&stdout, "summarize"), "{stdout}");
}

/// A fact saved in one session is found in another: what the model knows comes from the
/// harness, not from its weights or the conversation.
#[test]
#[ignore = "needs a local model: scripts/local-model.sh start, then cargo eval"]
fn remembers_across_sessions_through_the_memory_plugin() {
    let (workspace, agent) = workspace("eval-memory", &[]).unwrap();
    let first = run(
        &workspace,
        &agent,
        "tell",
        "Save this to memory for later sessions: our releases ship on Thursdays.",
    )
    .unwrap();
    let first = finished(&first).unwrap();
    assert!(succeeded(&first, "memory_save"), "{first}");

    let second = run(
        &workspace,
        &agent,
        "ask",
        "Check your memory: which day of the week do our releases ship?",
    )
    .unwrap();
    let second = finished(&second).unwrap();
    let answer = second
        .lines()
        .filter(|line| !line.starts_with('▸') && !line.starts_with("  "))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(answer.contains("Thursday"), "{first}\n---\n{second}");
}
