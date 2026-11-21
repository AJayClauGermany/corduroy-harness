//! Demo: a scripted model works through a small task using WebAssembly plugins.
//!
//! Run with `cargo plugins && cargo demo`. No network or LLM involved.

#![expect(
    clippy::multiple_crate_versions,
    reason = "the wasmtime and oi-kit dependency trees each pull several versions of shared crates"
)]

use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use corduroy_harness::core::Scope;
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::wasm::WasmRuntime;
use corduroy_harness::{Harness, shell_kernel};
use serde_json::json;

const PLUGINS: &str = "plugins/target/wasm32-unknown-unknown/release";

const SKILL: &str = "---\nname: count-lines\ndescription: Count the lines of a text file\n---\n\
                     ## Procedure\nRun `wc -l < FILE` in the shell.\n";

/// The model's side of the conversation.
fn script() -> ScriptedProvider {
    ScriptedProvider::new([
        call(
            "1",
            "write",
            &json!({"path": "notes.txt", "content": "one\ntwo\nthree\n"}),
        ),
        // The shell is a long-lived kernel: `lines` is still set in the next call.
        call(
            "2",
            "bash",
            &json!({"command": "lines=$(wc -l < notes.txt)"}),
        ),
        call(
            "3",
            "bash",
            &json!({"command": "echo \"notes.txt has $lines lines\""}),
        ),
        // Outside the plugin's scope: the host refuses and the model is told why.
        call("4", "read", &json!({"path": "/etc/hostname"})),
        call(
            "5",
            "skill_manage",
            &json!({"action": "create", "name": "count-lines", "content": SKILL}),
        ),
        say("notes.txt has 3 lines. I saved how to count them as the skill `count-lines`."),
    ])
}

/// A harness with the shell kernel and both WebAssembly plugins, confined to `work`.
fn harness(work: &Path) -> Result<Harness, Box<dyn Error>> {
    let workspace = Scope::subtree(&work.to_string_lossy());
    let skills = work.join("skills");
    let runtime = WasmRuntime::new()?.with_root(work);
    let load = |name: &str| {
        runtime
            .load(Path::new(PLUGINS).join(name))
            .map_err(|error| format!("{error}\nBuild the plugins first: cargo plugins"))
    };

    let mut harness =
        Harness::new(Arc::new(script()))?.with_system_prompt("You are a careful coding agent.");
    harness.plugin(shell_kernel("shell", work));
    harness.plugin(
        load("pi_tools.wasm")?
            .named("pi-tools")
            .grant("fs-read", workspace.clone())
            .grant("fs-write", workspace)
            .grant("kernel", Scope::none().allow("shell")),
    );
    harness.plugin(
        load("skills.wasm")?
            .grant("fs-read", Scope::subtree(&skills.to_string_lossy()))
            .grant("fs-write", Scope::subtree(&skills.to_string_lossy()))
            .grant("events", Scope::none().allow("skills/*"))
            .config(json!({"dir": skills, "nudge_interval": 3}).to_string()),
    );
    Ok(harness)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let work = std::env::temp_dir().join(format!("corduroy-demo-{}", std::process::id()));
    fs::create_dir_all(&work)?;
    let work = work.canonicalize()?;

    let harness = harness(&work)?;
    let outcome = harness
        .run("How many lines does notes.txt have? Remember how you found out.")
        .await?;

    let mut stdout = io::stdout().lock();
    // Messages and usage reports are on the stream too; they are left out to keep this short.
    let quiet = ["message", "usage"];
    for event in harness.events().events() {
        if quiet.contains(&event.topic.as_str()) {
            continue;
        }
        let line = event.to_string();
        let shown: String = line.chars().take(160).collect();
        let ellipsis = if shown.len() < line.len() { "…" } else { "" };
        writeln!(stdout, "{shown}{ellipsis}")?;
    }
    writeln!(stdout, "--\n{}", outcome.status)?;
    fs::remove_dir_all(&work)?;
    Ok(())
}
