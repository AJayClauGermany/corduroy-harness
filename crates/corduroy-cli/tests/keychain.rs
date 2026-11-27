//! Secrets kept in a keychain, through the command itself: a value typed in, seen to be
//! there, sent by a provider, and taken out again. The command runs in this process with a
//! keychain in memory in place of the system's, against a fake Responses endpoint on the
//! loopback interface. No network, no model, and nobody's real keychain.

#[path = "support/responses.rs"]
pub mod responses;

use std::fs;
use std::io::Cursor;
use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use corduroy_cli::command::{Console, run_on};
use corduroy_std::Standard;
use corduroy_std::keychain::{Keychain, Memory, Missing};
use corduroy_test_support::scratch;

use crate::responses::endpoint;

/// How a command ended, and everything it wrote.
struct Ran {
    /// Whether it exited with success. `Err` is what it would have said after `corduroy: `.
    ended: Result<bool, String>,
    /// What it wrote to its output.
    said: String,
}

impl Ran {
    /// Everything a person would have seen: the output, and the error if there was one.
    fn all(&self) -> String {
        match &self.ended {
            Ok(_) => self.said.clone(),
            Err(error) => format!("{}corduroy: {error}", self.said),
        }
    }

    fn succeeded(&self) -> bool {
        self.ended == Ok(true)
    }
}

/// Run the command in `workspace` with `args`, a person typing `typed`, and `standard`.
fn corduroy(standard: &Standard, workspace: &Path, args: &[&str], typed: &str) -> Ran {
    let written = Arc::new(Mutex::new(Vec::new()));
    let console = Console {
        input: Box::new(Cursor::new(typed.as_bytes().to_vec())),
        out: Arc::<Mutex<Vec<u8>>>::clone(&written),
        err: Arc::<Mutex<Vec<u8>>>::clone(&written),
        piped: false,
        // Nobody is at a terminal: a value is a line of what was typed.
        hidden: false,
    };
    let workspace = workspace.display().to_string();
    let args = ["--workspace", &workspace]
        .into_iter()
        .chain(args.iter().copied())
        .map(str::to_owned);
    let ended = run_on(standard, args, console)
        .map(|code| format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS));
    let said = String::from_utf8_lossy(
        &written
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
    .into_owned();
    Ran { ended, said }
}

/// The standard set with a keychain in memory, and that keychain.
fn with_keychain() -> (Standard, Arc<Memory>) {
    let keychain = Arc::new(Memory::new());
    let standard = Standard::new().with_keychain(Arc::<Memory>::clone(&keychain));
    (standard, keychain)
}

/// An agent whose provider, at `address`, sends the secret `token`, described by `secret`.
fn agent(address: SocketAddr, secret: &str) -> String {
    format!(
        "provider = \"plan\"\nmodel = \"gpt-6-fake\"\n\n[providers.plan]\nkind = \"openai\"\n\
         base_url = \"http://{address}/v1/chat/completions\"\nsecret = \"token\"\n\n\
         [secrets.token]\n{secret}\n\n[kernels.shell]\nkind = \"shell\"\n"
    )
}

#[test]
fn a_key_typed_into_the_keychain_is_what_the_provider_sends_and_is_shown_nowhere() {
    let workspace = scratch("cli-keychain-run");
    let (address, seen) = endpoint("Hello.").unwrap();
    fs::write(
        workspace.join("corduroy.toml"),
        agent(address, "kind = \"keychain\""),
    )
    .unwrap();
    let (standard, keychain) = with_keychain();

    // Typed (or pasted, with the space and line end pasting brings), it is kept.
    let set = corduroy(
        &standard,
        &workspace,
        &["secrets", "set", "token"],
        "  sk-typed \n",
    );
    assert!(set.succeeded(), "{}", set.all());
    assert_eq!(set.said, "kept `token` in a keychain in memory\n");
    assert_eq!(keychain.items(), ["token"]);
    assert_eq!(keychain.get("token").unwrap().as_deref(), Some("sk-typed"));

    let listed = corduroy(&standard, &workspace, &["secrets"], "");
    assert!(listed.succeeded(), "{}", listed.all());
    assert_eq!(
        listed.said,
        "✓ token (keychain, for provider plan): in a keychain in memory\n"
    );

    // A run sends it, and reads the keychain once however many requests it makes.
    let before = keychain.reads();
    let ran = corduroy(&standard, &workspace, &["run", "say hello"], "");
    assert!(ran.succeeded(), "{}", ran.all());
    assert!(ran.said.contains("Hello."), "{}", ran.said);
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests[0].authorization, "Bearer sk-typed");
    assert_eq!(requests[0].body["model"], "gpt-6-fake");
    assert_eq!(keychain.reads(), before + 1);

    // The value is in nothing the command printed, and not in the log.
    let log = corduroy(&standard, &workspace, &["log"], "");
    assert!(
        log.succeeded() && log.said.contains("say hello"),
        "{}",
        log.all()
    );
    for shown in [&set.said, &listed.said, &ran.said, &log.said] {
        assert!(!shown.contains("sk-typed"), "{shown}");
    }

    // Taken out again: the only secret kept there need not be named.
    let removed = corduroy(&standard, &workspace, &["secrets", "remove"], "");
    assert_eq!(removed.said, "removed the value of `token`\n");
    assert_eq!(keychain.items(), Vec::<String>::new());
    let again = corduroy(&standard, &workspace, &["secrets", "remove"], "");
    assert_eq!(again.said, "no value was kept for `token`\n");
}

#[test]
fn a_key_that_is_not_in_the_keychain_fails_the_run_and_says_the_command_that_puts_it_there() {
    let workspace = scratch("cli-keychain-empty");
    let (address, seen) = endpoint("Hello.").unwrap();
    fs::write(
        workspace.join("corduroy.toml"),
        agent(address, "kind = \"keychain\""),
    )
    .unwrap();
    let (standard, _keychain) = with_keychain();

    // Nothing is there yet: the list says so and what to do, and a run fails the same way
    // without anything being sent.
    let listed = corduroy(&standard, &workspace, &["secrets"], "");
    assert_eq!(listed.ended, Ok(false));
    assert_eq!(
        listed.said,
        "✗ token (keychain, for provider plan): a keychain in memory holds nothing as `token`: \
         put the value there with `corduroy secrets set token`\n"
    );
    let failed = corduroy(&standard, &workspace, &["run", "say hello"], "");
    assert_eq!(failed.ended, Ok(false), "{}", failed.all());
    assert!(
        failed.said.contains("secret `token`")
            && failed.said.contains("`corduroy secrets set token`"),
        "{}",
        failed.said
    );
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn a_key_is_not_signed_in_to_and_nothing_typed_or_unknown_changes_nothing() {
    let workspace = scratch("cli-keychain-login");
    let (address, _seen) = endpoint("Hello.").unwrap();
    fs::write(
        workspace.join("corduroy.toml"),
        agent(address, "kind = \"keychain\""),
    )
    .unwrap();
    let (standard, keychain) = with_keychain();

    // `login` is for what is signed in to. For a key it says the command that is.
    let said = "its value is put in with `corduroy secrets set token`";
    let login = corduroy(&standard, &workspace, &["login", "token"], "sk-by-login\n");
    assert_eq!(
        login.ended,
        Err(format!("`token` is no secret that is signed in to: {said}"))
    );
    let unnamed = corduroy(&standard, &workspace, &["login"], "sk-by-login\n");
    assert!(
        unnamed
            .ended
            .unwrap_err()
            .ends_with("the value of `token` is put in with `corduroy secrets set token`")
    );
    let logout = corduroy(&standard, &workspace, &["logout", "token"], "");
    assert!(logout.ended.unwrap_err().ends_with(said));
    assert_eq!(keychain.items(), Vec::<String>::new());

    // Nothing typed changes nothing, and says so.
    let empty = corduroy(&standard, &workspace, &["secrets", "set"], "\n");
    assert_eq!(
        empty.ended,
        Err("no value was given, so nothing was changed".to_owned())
    );
    assert_eq!(keychain.items(), Vec::<String>::new());
    // A name the file does not have is no item of the keychain either.
    let unknown = corduroy(&standard, &workspace, &["secrets", "set", "other"], "v\n");
    assert_eq!(
        unknown.ended,
        Err("the agent has no [secrets.other]".to_owned())
    );
    assert_eq!(keychain.items(), Vec::<String>::new());
}

#[test]
fn a_secret_that_says_nothing_about_itself_is_one_kept_in_the_keychain() {
    let workspace = scratch("cli-keychain-default");
    let (address, seen) = endpoint("Hello.").unwrap();
    // `[secrets.token]` and no more.
    fs::write(workspace.join("corduroy.toml"), agent(address, "")).unwrap();
    let (standard, keychain) = with_keychain();

    let listed = corduroy(&standard, &workspace, &["secrets"], "");
    assert!(
        listed.said.starts_with(
            "✗ token (keychain, for provider plan): a keychain in memory holds nothing"
        ),
        "{}",
        listed.all()
    );
    let set = corduroy(&standard, &workspace, &["secrets", "set"], "sk-default\n");
    assert_eq!(
        set.said,
        "kept `token` in a keychain in memory\n",
        "{}",
        set.all()
    );
    assert_eq!(keychain.items(), ["token"]);
    let ran = corduroy(&standard, &workspace, &["run", "say hello"], "");
    assert!(ran.succeeded(), "{}", ran.all());
    assert_eq!(seen.lock().unwrap()[0].authorization, "Bearer sk-default");
}

#[test]
fn a_secret_of_another_kind_is_not_put_in_and_the_message_says_where_its_value_goes() {
    let workspace = scratch("cli-keychain-env");
    let (address, _seen) = endpoint("Hello.").unwrap();
    fs::write(
        workspace.join("corduroy.toml"),
        agent(address, "env = \"CORDUROY_TEST_NEVER_SET\""),
    )
    .unwrap();
    let (standard, keychain) = with_keychain();

    let listed = corduroy(&standard, &workspace, &["secrets"], "");
    assert_eq!(listed.ended, Ok(false));
    assert_eq!(
        listed.said,
        "✗ token (env, for provider plan): $CORDUROY_TEST_NEVER_SET is not set\n"
    );
    let set = corduroy(&standard, &workspace, &["secrets", "set", "token"], "v\n");
    let error = set.ended.unwrap_err();
    assert!(
        error.starts_with("the value of `token` is not put in with this command")
            && error.contains("kind = \"keychain\""),
        "{error}"
    );
    // With none named and none kept in the keychain, what to add is said.
    let none = corduroy(&standard, &workspace, &["secrets", "set"], "v\n");
    assert!(
        none.ended
            .unwrap_err()
            .contains("[secrets.anthropic]\nkind = \"keychain\"")
    );
    assert_eq!(keychain.items(), Vec::<String>::new());
}

#[test]
fn on_a_machine_without_a_keychain_nothing_is_kept_anywhere_else() {
    let workspace = scratch("cli-keychain-missing");
    let (address, seen) = endpoint("Hello.").unwrap();
    fs::write(
        workspace.join("corduroy.toml"),
        agent(address, "kind = \"keychain\""),
    )
    .unwrap();
    let standard = Standard::new().with_keychain(Missing::new("nobody is logged in"));
    let none = "this machine has no keychain (nobody is logged in)";

    let listed = corduroy(&standard, &workspace, &["secrets"], "");
    assert_eq!(listed.ended, Ok(false));
    assert!(listed.said.contains(none), "{}", listed.said);
    let set = corduroy(&standard, &workspace, &["secrets", "set"], "sk-typed\n");
    assert_eq!(set.ended, Err(none.to_owned()));
    let ran = corduroy(&standard, &workspace, &["run", "say hello"], "");
    assert_eq!(ran.ended, Ok(false));
    assert!(ran.said.contains(none), "{}", ran.said);
    assert!(seen.lock().unwrap().is_empty());
    // Nothing was written beside the agent file but the agent's own directory.
    let beside: Vec<String> = fs::read_dir(&workspace)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        beside
            .iter()
            .all(|name| name == "corduroy.toml" || name == ".corduroy")
    );
}

/// The agent file `init` wrote in `workspace`.
fn written(workspace: &Path) -> String {
    fs::read_to_string(workspace.join("corduroy.toml")).unwrap_or_default()
}

#[test]
fn init_writes_keys_into_the_keychain_where_there_is_one_and_the_environment_where_not() {
    let (standard, keychain) = with_keychain();

    // A keychain: the entry names it, and the person is told the one command to run.
    let kept = scratch("cli-init-keychain");
    let init = corduroy(&standard, &kept, &["init", "anthropic"], "");
    assert!(init.succeeded(), "{}", init.all());
    let file = written(&kept);
    assert!(
        file.contains("[secrets.anthropic]\nkind = \"keychain\"\n"),
        "{file}"
    );
    assert!(file.contains("secret = \"anthropic\""), "{file}");
    assert!(
        init.said.contains(
            "put the key `anthropic` in a keychain in memory:  corduroy secrets set anthropic"
        ) && init.said.contains("corduroy doctor"),
        "{}",
        init.said
    );
    // It only writes where a key comes from: nothing is put anywhere.
    assert_eq!(keychain.items(), Vec::<String>::new());
    // What it wrote is an agent the command accepts, whose secret is listed as missing.
    let listed = corduroy(&standard, &kept, &["secrets"], "");
    assert!(
        listed
            .said
            .contains("✗ anthropic (keychain, for provider main)"),
        "{}",
        listed.all()
    );

    // A file that is there is left alone.
    let again = corduroy(&standard, &kept, &["init"], "");
    assert!(
        again
            .ended
            .unwrap_err()
            .ends_with("corduroy.toml already exists")
    );
}

#[test]
fn init_on_a_machine_without_a_keychain_writes_the_environment_and_says_why() {
    let without = Standard::new().with_keychain(Missing::new("nobody is logged in"));

    // No keychain: the environment, and why. A sign-in is then kept in a file.
    let bare = scratch("cli-init-environment");
    let init = corduroy(&without, &bare, &["init", "anthropic"], "");
    assert!(init.succeeded(), "{}", init.all());
    let file = written(&bare);
    assert!(
        file.contains("[secrets.anthropic]\nenv = \"ANTHROPIC_API_KEY\"\n"),
        "{file}"
    );
    assert!(
        init.said.contains(
            "this machine has no keychain (nobody is logged in), so its keys are read from the \
             environment"
        ),
        "{}",
        init.said
    );

    let signed = scratch("cli-init-plan-file");
    let init = corduroy(&without, &signed, &["init", "chatgpt"], "");
    assert!(
        init.said
            .contains("so the sign-in is kept in a file of your own"),
        "{}",
        init.said
    );
    assert!(written(&signed).contains("[secrets.chatgpt]\nkind = \"chatgpt\"\nkeep = \"file\"\n"));

    // Asked for and not there: refused, and nothing written in its place.
    let refused = scratch("cli-init-refused");
    let init = corduroy(
        &without,
        &refused,
        &["init", "anthropic", "--secrets", "keychain"],
        "",
    );
    assert!(
        init.ended
            .unwrap_err()
            .contains("this machine has no keychain")
    );
    assert_eq!(written(&refused), "");
}

#[test]
fn init_takes_the_vendor_and_where_keys_come_from_as_said() {
    let (standard, _keychain) = with_keychain();

    // The environment by choice, where there is a keychain.
    let chosen = scratch("cli-init-chosen");
    let init = corduroy(
        &standard,
        &chosen,
        &["init", "openai", "--secrets", "env"],
        "",
    );
    assert!(init.succeeded(), "{}", init.all());
    assert!(written(&chosen).contains("[secrets.openai]\nenv = \"OPENAI_API_KEY\"\n"));

    // A ChatGPT plan: a sign-in kept in the keychain, and the command that makes it.
    let paid = scratch("cli-init-plan");
    let init = corduroy(&standard, &paid, &["init", "chatgpt"], "");
    assert!(init.succeeded(), "{}", init.all());
    let file = written(&paid);
    assert!(
        file.contains("[secrets.chatgpt]\nkind = \"chatgpt\"\n\n") && !file.contains("keep ="),
        "{file}"
    );
    assert!(
        init.said.contains("corduroy login chatgpt"),
        "{}",
        init.said
    );
    let listed = corduroy(&standard, &paid, &["secrets"], "");
    assert!(
        listed.said.contains(
            "✗ chatgpt (chatgpt, for provider main): nobody is signed in to ChatGPT (looked in \
             a keychain in memory): run `corduroy login chatgpt`"
        ),
        "{}",
        listed.all()
    );
}

#[test]
fn doctor_tries_each_secret_kernel_and_model_and_says_which_does_not_work() {
    let workspace = scratch("cli-doctor");
    let (address, seen) = endpoint("ok").unwrap();
    fs::write(
        workspace.join("corduroy.toml"),
        agent(address, "kind = \"keychain\""),
    )
    .unwrap();
    let (standard, keychain) = with_keychain();

    // Without the key: the secret and the model that needs it fail, the kernel works.
    let sick = corduroy(&standard, &workspace, &["doctor"], "");
    assert_eq!(sick.ended, Ok(false), "{}", sick.all());
    let lines: Vec<&str> = sick.said.lines().collect();
    assert!(
        lines[0].starts_with("✓ agent    ") && lines[0].ends_with("everything it lists started")
    );
    assert!(
        lines[1].starts_with(
            "✗ secret   token (keychain, for provider plan): a keychain in memory holds nothing"
        ),
        "{}",
        sick.said
    );
    assert!(
        lines[2].starts_with("✓ kernel   shell: ran a command in "),
        "{}",
        sick.said
    );
    assert!(
        lines[3].starts_with("✗ model    plan/gpt-6-fake: ") && lines[3].contains("secret `token`"),
        "{}",
        sick.said
    );
    assert_eq!(lines[4], "2 of the 4 parts tried are not working");
    assert!(seen.lock().unwrap().is_empty());

    // With it: everything works, and the model was asked with the key.
    keychain.set("token", "sk-kept").unwrap();
    let well = corduroy(&standard, &workspace, &["doctor"], "");
    assert!(well.succeeded(), "{}", well.all());
    assert!(
        well.said
            .contains("✓ secret   token (keychain, for provider plan): in a keychain in memory")
            && well
                .said
                .contains("✓ model    plan/gpt-6-fake: answered in ")
            && well.said.contains("(5 tokens in, 2 out)")
            && well.said.ends_with("all 4 parts tried are working\n"),
        "{}",
        well.said
    );
    assert_eq!(seen.lock().unwrap()[0].authorization, "Bearer sk-kept");
    assert!(!well.said.contains("sk-kept"));
}
