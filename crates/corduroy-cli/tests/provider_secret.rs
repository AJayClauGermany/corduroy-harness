//! A provider whose credential is a secret: the `corduroy` binary against a fake Responses
//! endpoint on the loopback interface, which keeps what it was sent. No network, no model.

#[path = "support/responses.rs"]
pub mod responses;

use std::fs;
use std::io::Write as _;
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::{Command, Output};
use std::thread;

use corduroy_test_support::scratch;
use serde_json::{Value, json};

use crate::responses::{endpoint, read};

/// An endpoint that answers every request with `status` and `body`.
fn refusal(status: &'static str, body: &Value) -> std::io::Result<SocketAddr> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let body = body.to_string();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _sent = read(&stream);
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
            let _written = (&stream).write_all(response.as_bytes());
        }
    });
    Ok(address)
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

/// An agent file whose provider is paid by a `ChatGPT` plan.
fn plan_file(address: SocketAddr) -> String {
    format!(
        "provider = \"plan\"\nmodel = \"gpt-6-fake\"\n\n[providers.plan]\nkind = \"openai\"\n\
         base_url = \"http://{address}/v1/chat/completions\"\nsecret = \"chatgpt\"\n\n\
         [secrets.chatgpt]\nkind = \"chatgpt\"\n"
    )
}

/// Keep a sign-in under `own`, the person's own directory, as the secret `chatgpt`: good for
/// an hour from now.
fn sign_in(own: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let kept = own.join("corduroy").join("chatgpt");
    fs::create_dir_all(&kept)?;
    let record = json!({
        "email": "person@example.com", "issuer": "https://auth.openai.com",
        "subject": "user-1", "client_id": "oaiapp_1", "ext_agent_host_id": "urn:uuid:0",
        "id_token": "id", "access_token": "plan-t0ken", "refresh_token": "refresh-t0ken",
        "token_type": "Bearer", "expires_in": 3600, "saved_at": now,
        "scopes": ["openid", "chatgpt.tokens.use.direct"],
    });
    fs::write(kept.join("chatgpt.json"), record.to_string())?;
    Ok(())
}

#[test]
fn a_failure_of_the_plan_is_followed_by_what_the_person_can_do() {
    let workspace = scratch("cli-chatgpt-refused");
    let own = scratch("cli-chatgpt-refused-own");
    let own_text = own.to_string_lossy().into_owned();
    let env = [
        ("XDG_CONFIG_HOME", own_text.as_str()),
        ("APPDATA", own_text.as_str()),
    ];
    sign_in(&own).unwrap();

    // The service refuses the sign-in: the run fails with its reason, then what to do.
    let refusing = refusal(
        "401 Unauthorized",
        &json!({"error": {"message": "not accepted", "code": "subscription_sharing_invalid_user"}}),
    )
    .unwrap();
    fs::write(workspace.join("corduroy.toml"), plan_file(refusing)).unwrap();
    let output = corduroy(&workspace, &["run", "say hello"], &env).unwrap();
    let said = format!("{}{}", text(&output.stdout), text(&output.stderr));
    assert!(!output.status.success(), "{said}");
    assert!(
        said.contains("the run failed") && said.contains("not accepted"),
        "{said}"
    );
    assert!(
        said.contains("sign in again with `corduroy login`"),
        "{said}"
    );
    assert!(!said.contains("t0ken"), "{said}");
}

#[test]
fn a_chatgpt_sign_in_is_the_token_a_provider_sends_and_its_absence_names_the_command() {
    let workspace = scratch("cli-chatgpt");
    let own = scratch("cli-chatgpt-own");
    let (address, seen) = endpoint("Hello.").unwrap();
    fs::write(workspace.join("corduroy.toml"), plan_file(address)).unwrap();
    // The person's own directory, wherever this system keeps it.
    let own_text = own.to_string_lossy().into_owned();
    let env = [
        ("XDG_CONFIG_HOME", own_text.as_str()),
        ("APPDATA", own_text.as_str()),
    ];

    // Nobody is signed in: the run fails, saying how to, and nothing is sent.
    let output = corduroy(&workspace, &["run", "say hello"], &env).unwrap();
    let said = format!("{}{}", text(&output.stdout), text(&output.stderr));
    assert!(
        !output.status.success() && said.contains("corduroy login"),
        "{said}"
    );
    assert!(seen.lock().unwrap().is_empty());

    sign_in(&own).unwrap();

    let output = corduroy(&workspace, &["run", "say hello"], &env).unwrap();
    let stdout = text(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        text(&output.stderr)
    );
    let sent = seen.lock().unwrap().first().cloned().unwrap();
    assert_eq!(sent.authorization, "Bearer plan-t0ken");
    // The person is told whose account pays, and where to see what it has used.
    assert!(
        stdout.contains("Using ChatGPT plan. Manage usage: https://chatgpt.com/settings/usage"),
        "{stdout}"
    );
    let log = text(&corduroy(&workspace, &["log"], &env).unwrap().stdout);
    assert!(!log.contains("t0ken") && !stdout.contains("t0ken"));
}

#[test]
fn signing_in_needs_a_secret_that_is_signed_in_to() {
    let workspace = scratch("cli-login-none");
    fs::write(
        workspace.join("corduroy.toml"),
        "provider = \"p\"\nmodel = \"m\"\n\n[providers.p]\nkind = \"ollama\"\n\n\
         [secrets.token]\nenv = \"SOME_TOKEN\"\n",
    )
    .unwrap();
    let output = corduroy(&workspace, &["login"], &[]).unwrap();
    let said = text(&output.stderr);
    assert!(!output.status.success());
    assert!(said.contains("kind = \"chatgpt\""), "{said}");

    // A kind the command does not have is refused when the agent is put together.
    fs::write(
        workspace.join("corduroy.toml"),
        "provider = \"p\"\nmodel = \"m\"\n\n[providers.p]\nkind = \"ollama\"\n\n\
         [secrets.token]\nkind = \"vault\"\n",
    )
    .unwrap();
    let output = corduroy(&workspace, &["check"], &[]).unwrap();
    let said = format!("{}{}", text(&output.stdout), text(&output.stderr));
    assert!(said.contains("kind `vault`"), "{said}");
}
