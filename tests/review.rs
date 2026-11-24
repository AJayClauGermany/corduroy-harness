//! The `review` add-on through the loop: the change in a git repository, sent to a second
//! model, whose answer the first one gets. Build the add-ons first with `cargo plugins`.
//! Needs `git`; no network or LLM.

use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use corduroy_harness::core::{Pattern, Scope};
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::wasm::WasmRuntime;
use corduroy_harness::{Harness, provider};
use corduroy_shell::shell_kernel;
use corduroy_test_support::{plugin, scratch};
use serde_json::{Value, json};

/// Run `git args` in `work`.
fn git(work: &Path, args: &[&str]) -> Result<(), Box<dyn Error>> {
    let status = Command::new("git")
        .arg("-C")
        .arg(work)
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(args)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("git {args:?} failed: {status}").into())
    }
}

/// A repository in `work` with one commit of `lib.rs`.
fn repository(work: &Path) -> Result<(), Box<dyn Error>> {
    git(work, &["init", "--quiet"])?;
    fs::write(
        work.join("lib.rs"),
        "pub fn add(a: u8, b: u8) -> u8 {\n    a + b\n}\n",
    )?;
    git(work, &["add", "lib.rs"])?;
    git(work, &["commit", "--quiet", "-m", "start"])
}

/// A harness asking `main`, with a shell kernel in `work`, a `helper` provider serving the
/// model `reviewer` and the `review` add-on asking it.
fn agent(
    work: &Path,
    main: &Arc<ScriptedProvider>,
    helper: &Arc<ScriptedProvider>,
) -> Result<Harness, Box<dyn Error>> {
    let harness = Harness::new(Arc::<ScriptedProvider>::clone(main))?;
    harness.plugin(shell_kernel("shell", work));
    harness
        .plugin(provider("helper", Arc::<ScriptedProvider>::clone(helper)).serving(["reviewer"]));
    harness.plugin(
        WasmRuntime::new()?
            .with_root(work)
            .load(plugin("review"))?
            .grant("fs-read", Scope::subtree(&work.to_string_lossy()))
            .grant("kernel", Scope::none().allow("shell"))
            .grant("llm", Scope::none().allow("reviewer"))
            .config(
                json!({"model": "reviewer", "dir": work, "instructions": "Mind overflow."})
                    .to_string(),
            ),
    );
    Ok(harness)
}

/// The output of every tool call so far, in order.
fn outputs(harness: &Harness) -> Vec<String> {
    harness
        .events()
        .matching(&Pattern::new("tool/result"))
        .iter()
        .filter_map(|event| serde_json::from_str::<Value>(&event.payload).ok())
        .filter_map(|result| Some(result.get("output")?.as_str()?.to_owned()))
        .collect()
}

#[tokio::test]
async fn the_change_and_new_files_go_to_the_other_model_and_its_answer_comes_back() {
    let work = scratch("review-change");
    repository(&work).unwrap();
    fs::write(
        work.join("lib.rs"),
        "pub fn add(a: u8, b: u8) -> u8 {\n    a.wrapping_add(b)\n}\n",
    )
    .unwrap();
    fs::write(work.join("new.rs"), "pub fn sub() {}\n").unwrap();
    let main = Arc::new(ScriptedProvider::new([
        call("1", "review", &json!({"goal": "make add total"})),
        say("fixed"),
    ]));
    let helper = Arc::new(ScriptedProvider::new([say("lib.rs:2 wraps silently.")]));
    let harness = agent(&work, &main, &helper).unwrap();

    harness.run("add numbers safely").await.unwrap();

    assert_eq!(
        outputs(&harness)[0],
        "Review by reviewer of the changes against `HEAD`:\n\nlib.rs:2 wraps silently."
    );
    let asked = &helper.requests()[0];
    assert!(
        asked.system.ends_with("\n\nMind overflow."),
        "{}",
        asked.system
    );
    // What the add-on sent, as the host recorded it.
    let sent = &harness.events().matching(&Pattern::new("llm/request"))[0].payload;
    assert!(
        sent.contains("The change is meant to: make add total"),
        "{sent}"
    );
    assert!(sent.contains("-    a + b"), "{sent}");
    assert!(sent.contains("+    a.wrapping_add(b)"), "{sent}");
    assert!(
        sent.contains("=== new.rs (whole file) ===\\npub fn sub() {}"),
        "{sent}"
    );
}

#[tokio::test]
async fn nothing_changed_means_nothing_is_asked() {
    let work = scratch("review-nothing");
    repository(&work).unwrap();
    let main = Arc::new(ScriptedProvider::new([
        call("1", "review", &json!({})),
        say("done"),
    ]));
    let helper = Arc::new(ScriptedProvider::new([]));
    let harness = agent(&work, &main, &helper).unwrap();

    harness.run("check").await.unwrap();

    assert_eq!(
        outputs(&harness)[0],
        "There are no changes against `HEAD` to review."
    );
    assert_eq!(helper.requests().len(), 0);
}

#[tokio::test]
async fn a_base_that_is_not_a_ref_never_reaches_the_shell() {
    let work = scratch("review-base");
    repository(&work).unwrap();
    let main = Arc::new(ScriptedProvider::new([
        call("1", "review", &json!({"base": "HEAD; touch pwned"})),
        say("done"),
    ]));
    let helper = Arc::new(ScriptedProvider::new([]));
    let harness = agent(&work, &main, &helper).unwrap();

    harness.run("check").await.unwrap();

    assert!(
        outputs(&harness)[0].contains("is not a commit, branch or tag"),
        "{}",
        outputs(&harness)[0]
    );
    assert!(!work.join("pwned").exists());
    assert_eq!(helper.requests().len(), 0);
}
