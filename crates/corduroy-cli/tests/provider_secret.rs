//! A provider whose credential is a secret: the `corduroy` binary against a fake Responses
//! endpoint on the loopback interface, which keeps what it was sent. No network, no model.

use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;

use corduroy_test_support::scratch;
use serde_json::{Value, json};

/// One request as the endpoint saw it: its `Authorization` header and its body.
#[derive(Debug, Clone)]
struct Sent {
    authorization: String,
    body: Value,
}

fn read(stream: &TcpStream) -> Sent {
    let mut reader = BufReader::new(stream);
    let (mut length, mut authorization) = (0, String::new());
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
        let lower = line.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("authorization:") {
            authorization = line
                .split_once(':')
                .map(|(_, value)| value.trim().to_owned())
                .unwrap_or_default();
        }
        line.clear();
    }
    let mut body = vec![0; length];
    let _read = reader.read_exact(&mut body);
    Sent {
        authorization,
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    }
}

/// A Responses stream answering with `text`.
fn stream_of(text: &str) -> String {
    let item = json!({
        "type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
        "content": [{"type": "output_text", "text": text, "annotations": []}],
    });
    let response = |status: &str, output: Value| {
        json!({
            "id": "resp_1", "object": "response", "status": status, "model": "gpt-6-fake",
            "output": output,
            "usage": {"input_tokens": 5, "output_tokens": 2, "total_tokens": 7},
        })
    };
    let events = [
        (
            "response.created",
            json!({"response": response("in_progress", json!([]))}),
        ),
        (
            "response.output_item.added",
            json!({"output_index": 0, "item": item}),
        ),
        (
            "response.output_text.delta",
            json!({"item_id": "msg_1", "output_index": 0, "content_index": 0, "delta": text}),
        ),
        (
            "response.output_item.done",
            json!({"output_index": 0, "item": item}),
        ),
        (
            "response.completed",
            json!({"response": response("completed", json!([item]))}),
        ),
    ];
    let mut stream = String::new();
    for (number, (kind, mut event)) in events.into_iter().enumerate() {
        if let Value::Object(fields) = &mut event {
            fields.insert("type".to_owned(), json!(kind));
            fields.insert("sequence_number".to_owned(), json!(number));
        }
        let _written = write!(stream, "event: {kind}\ndata: {event}\n\n");
    }
    stream
}

/// The requests an endpoint received, in order.
type Seen = Arc<Mutex<Vec<Sent>>>;

/// A Responses endpoint that answers every request with `text`, and keeps each request.
fn endpoint(text: &'static str) -> std::io::Result<(SocketAddr, Seen)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&seen);
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let sent = read(&stream);
            if let Ok(mut seen) = kept.lock() {
                seen.push(sent);
            }
            let body = stream_of(text);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
            let _written = (&stream).write_all(response.as_bytes());
        }
    });
    Ok((address, seen))
}

fn corduroy(workspace: &Path, args: &[&str], env: &[(&str, &str)]) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_corduroy"))
        .arg("--workspace")
        .arg(workspace)
        .args(args)
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .envs(env.iter().copied())
        .output()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn a_provider_sends_the_secret_it_names_and_the_system_prompt_as_instructions() {
    let workspace = scratch("cli-provider-secret");
    let (address, seen) = endpoint("Hello.").unwrap();
    fs::write(
        workspace.join("corduroy.toml"),
        format!(
            r#"
system = "Be brief."
provider = "plan"
model = "gpt-6-fake"

[providers.plan]
kind = "openai"
base_url = "http://{address}/v1/chat/completions"
secret = "token"

[secrets.token]
env = "CORDUROY_TEST_PLAN_TOKEN"
"#
        ),
    )
    .unwrap();

    let env = [("CORDUROY_TEST_PLAN_TOKEN", "t0ken")];
    let output = corduroy(&workspace, &["run", "say hello"], &env).unwrap();
    let stdout = text(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        text(&output.stderr)
    );
    assert!(stdout.contains("Hello."), "{stdout}");

    let seen = seen.lock().unwrap().clone();
    let sent = &seen[0];
    // The secret's value, and no key from the command's own environment.
    assert_eq!(sent.authorization, "Bearer t0ken");
    // The system prompt is `instructions`, no `system` message is in the input, and no
    // limit is sent where the entry states none.
    let instructions = sent.body["instructions"].as_str().unwrap();
    assert!(instructions.starts_with("Be brief."), "{instructions}");
    let roles: Vec<&str> = sent.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["role"].as_str())
        .collect();
    assert_eq!(roles, ["user"]);
    assert!(
        sent.body.get("max_output_tokens").is_none(),
        "{}",
        sent.body
    );
    assert_eq!(sent.body["store"], false);

    // The value is in neither the log nor what the command printed.
    let log = text(&corduroy(&workspace, &["log"], &env).unwrap().stdout);
    assert!(!log.contains("t0ken") && !stdout.contains("t0ken"));
}

#[test]
fn a_secret_that_cannot_be_revealed_fails_the_run_by_name_and_a_missing_one_the_check() {
    let workspace = scratch("cli-provider-secret-missing");
    let (address, seen) = endpoint("Hello.").unwrap();
    let file = |secrets: &str| {
        format!(
            "provider = \"plan\"\nmodel = \"gpt-6-fake\"\n\n[providers.plan]\nkind = \"openai\"\n\
             base_url = \"http://{address}/v1/chat/completions\"\nsecret = \"token\"\n{secrets}"
        )
    };

    // The variable is not set: the run fails saying which secret, and nothing is sent.
    fs::write(
        workspace.join("corduroy.toml"),
        file("\n[secrets.token]\nenv = \"CORDUROY_TEST_PLAN_TOKEN_UNSET\"\n"),
    )
    .unwrap();
    let output = corduroy(&workspace, &["run", "say hello"], &[]).unwrap();
    let said = format!("{}{}", text(&output.stdout), text(&output.stderr));
    assert!(said.contains("secret `token`"), "{said}");
    assert!(seen.lock().unwrap().is_empty());

    // No such entry at all: the agent file is refused.
    fs::write(workspace.join("corduroy.toml"), file("")).unwrap();
    let output = corduroy(&workspace, &["check"], &[]).unwrap();
    assert!(!output.status.success());
    let said = format!("{}{}", text(&output.stdout), text(&output.stderr));
    assert!(said.contains("there is no [secrets.token]"), "{said}");
}
