//! The loop with the WebAssembly plugins from `plugins/`. Build them first: `cargo plugins`.
//! No network or LLM.

use std::error::Error;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use corduroy_harness::core::{FiberId, Pattern, Scope};
use corduroy_harness::oi_kit_provider_types::conversation::message::Message;
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::wasm::WasmRuntime;
use corduroy_harness::{Harness, Status, shell_kernel};
use corduroy_test_support::{plugin, scratch};
use serde_json::json;

const SKILL: &str = "---\nname: count-lines\ndescription: Count the lines of a text file\n---\n\
                     Run `wc -l < FILE`.\n";

struct Agent {
    harness: Harness,
    model: Arc<ScriptedProvider>,
    kernel: FiberId,
    skills: FiberId,
}

/// A harness confined to `work`: a shell kernel, `pi-tools` over the whole directory and
/// `skills` over `work/skills` only.
fn agent(work: &Path, replies: impl IntoIterator<Item = Message>) -> Result<Agent, Box<dyn Error>> {
    let model = Arc::new(ScriptedProvider::new(replies));
    let provider: Arc<ScriptedProvider> = Arc::clone(&model);
    let mut harness = Harness::new(provider)?.with_system_prompt("Be brief.");
    let runtime = WasmRuntime::new()?.with_root(work);
    let workspace = Scope::subtree(&work.to_string_lossy());
    let skills_dir = work.join("skills");
    let skills_scope = Scope::subtree(&skills_dir.to_string_lossy());

    let kernel = harness.plugin(shell_kernel("shell", work));
    harness.plugin(
        runtime
            .load(plugin("pi-tools"))?
            .named("pi-tools")
            .grant("fs-read", workspace.clone())
            .grant("fs-write", workspace)
            .grant("kernel", Scope::none().allow("shell")),
    );
    let skills = harness.plugin(
        runtime
            .load(plugin("skills"))?
            .grant("fs-read", skills_scope.clone())
            .grant("fs-write", skills_scope)
            .grant("events", Scope::none().allow("skills/*"))
            .config(json!({"dir": skills_dir, "nudge_interval": 2}).to_string()),
    );
    Ok(Agent {
        harness,
        model,
        kernel,
        skills,
    })
}

fn payloads(harness: &Harness, topics: &str) -> Vec<String> {
    harness
        .events()
        .matching(&Pattern::new(topics))
        .into_iter()
        .map(|event| event.payload)
        .collect()
}

/// The output of every tool call so far, in order.
fn outputs(harness: &Harness) -> Vec<String> {
    payloads(harness, "tool/result")
        .iter()
        .filter_map(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
        .filter_map(|result| Some(result.get("output")?.as_str()?.to_owned()))
        .collect()
}

#[tokio::test]
async fn a_task_runs_across_both_plugins_within_their_grants() {
    let work = scratch("task");
    let agent = agent(
        &work,
        [
            call(
                "1",
                "write",
                &json!({"path": "notes.txt", "content": "one\ntwo\nthree\n"}),
            ),
            call(
                "2",
                "bash",
                &json!({"command": "lines=$(wc -l < notes.txt)"}),
            ),
            call("3", "bash", &json!({"command": "echo $lines"})),
            call("4", "read", &json!({"path": "/etc/hostname"})),
            call(
                "5",
                "skill_manage",
                &json!({"action": "create", "name": "count-lines", "content": SKILL}),
            ),
            say("three lines"),
        ],
    )
    .unwrap();

    let outcome = agent.harness.run("how long is notes.txt?").await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "three lines".into()
        }
    );
    assert_eq!(
        outputs(&agent.harness),
        [
            "Wrote 14 bytes to notes.txt",
            "",
            "3\n",
            "permission denied: capability `fs-read` does not cover `/etc/hostname`",
            "Created skill `count-lines`.",
        ]
    );
    assert_eq!(
        fs::read_to_string(work.join("skills/count-lines/SKILL.md")).unwrap(),
        SKILL
    );
    assert_eq!(
        agent.model.requests()[0].tools,
        [
            "bash",
            "edit",
            "read",
            "skill_manage",
            "skill_view",
            "skills_list",
            "write"
        ]
    );
}

#[tokio::test]
async fn denials_and_plugin_events_are_on_the_stream() {
    let work = scratch("stream");
    let agent = agent(
        &work,
        [
            call("1", "read", &json!({"path": "/etc/hostname"})),
            call(
                "2",
                "skill_manage",
                &json!({"action": "create", "name": "count-lines", "content": SKILL}),
            ),
            say("ok"),
        ],
    )
    .unwrap();

    agent.harness.run("try things").await.unwrap();

    assert_eq!(
        payloads(&agent.harness, "capability/denied"),
        [r#"{"plugin":"pi-tools","capability":"fs-read","resource":"/etc/hostname"}"#]
    );
    assert_eq!(
        payloads(&agent.harness, "skills/changed"),
        [r#"{"action":"create","name":"count-lines"}"#]
    );
}

#[tokio::test]
async fn saved_skills_are_indexed_in_the_prompt_and_loaded_on_demand() {
    let work = scratch("skills-index");
    let agent = agent(
        &work,
        [
            call("1", "skill_manage", &json!({"action": "create", "name": "count-lines", "content": SKILL})),
            say("saved"),
            call("2", "skill_view", &json!({"name": "count-lines"})),
            call(
                "3",
                "skill_manage",
                &json!({"action": "patch", "name": "count-lines", "old_string": "wc -l < FILE", "new_string": "wc -l FILE"}),
            ),
            say("patched"),
        ],
    )
    .unwrap();

    agent
        .harness
        .run("remember how to count lines")
        .await
        .unwrap();
    agent
        .harness
        .run("the redirect is unnecessary; fix the skill")
        .await
        .unwrap();

    let requests = agent.model.requests();
    assert!(requests[0].system.starts_with("Be brief.\n\n"));
    assert!(
        requests[0]
            .system
            .contains("<available_skills>\n(none yet)\n")
    );
    // Only the name and description are in the prompt; the body is fetched with skill_view.
    assert!(
        requests[2]
            .system
            .contains("- count-lines: Count the lines of a text file\n")
    );
    assert!(!requests[2].system.contains("wc -l"));
    assert_eq!(outputs(&agent.harness)[1], SKILL);
    assert!(
        fs::read_to_string(work.join("skills/count-lines/SKILL.md"))
            .unwrap()
            .contains("Run `wc -l FILE`.")
    );
}

#[tokio::test]
async fn the_skills_plugin_cannot_reach_outside_its_directory() {
    let work = scratch("skills-scope");
    fs::write(work.join("notes.txt"), "private").unwrap();
    let agent = agent(
        &work,
        [
            call("1", "skill_view", &json!({"name": "../notes.txt"})),
            call(
                "2",
                "skill_manage",
                &json!({"action": "create", "name": "x", "content": "no front matter"}),
            ),
            say("gave up"),
        ],
    )
    .unwrap();

    agent.harness.run("poke around").await.unwrap();

    let outputs = outputs(&agent.harness);
    assert!(outputs[0].starts_with("invalid input: `../notes.txt` is not a valid skill name"));
    assert_eq!(
        outputs[1],
        "invalid input: a skill must start with a `---` front matter line"
    );
    assert!(!work.join("skills/x").exists());
}

#[tokio::test]
async fn enough_work_without_saving_a_skill_triggers_a_review_nudge() {
    let work = scratch("nudge");
    let agent = agent(
        &work,
        [
            call("1", "bash", &json!({"command": "true"})),
            call("2", "bash", &json!({"command": "true"})),
            say("done"),
            call(
                "3",
                "skill_manage",
                &json!({"action": "create", "name": "count-lines", "content": SKILL}),
            ),
            say("saved"),
            say("nothing to do"),
        ],
    )
    .unwrap();

    agent.harness.run("do some work").await.unwrap();
    assert_eq!(
        payloads(&agent.harness, "skills/review-due"),
        [r#"{"tool_calls":2}"#]
    );
    agent.harness.run("anything worth keeping?").await.unwrap();
    agent.harness.run("and now?").await.unwrap();

    let requests = agent.model.requests();
    assert!(!requests[0].system.contains("<skill_review>"));
    assert!(requests[3].system.contains("<skill_review>"));
    // Saving a skill answers the nudge.
    assert!(!requests[5].system.contains("<skill_review>"));
}

#[tokio::test]
async fn reloading_a_plugin_starts_it_from_fresh_memory() {
    let work = scratch("reload");
    let mut agent = agent(
        &work,
        [
            call("1", "bash", &json!({"command": "true"})),
            call("2", "bash", &json!({"command": "true"})),
            say("done"),
            say("hello again"),
        ],
    )
    .unwrap();

    agent.harness.run("do some work").await.unwrap();
    agent.harness.app().reload(agent.skills).unwrap();
    agent.harness.run("hello").await.unwrap();

    // The nudge was due, but that lived in the old instance's memory.
    assert!(!agent.model.requests()[3].system.contains("<skill_review>"));
    assert_eq!(payloads(&agent.harness, "plugin/suspended").len(), 1);
}

#[tokio::test]
async fn a_kernel_lasts_as_long_as_the_plugin_that_provides_it() {
    let work = scratch("kernel-plugin");
    let mut agent = agent(
        &work,
        [
            call("1", "bash", &json!({"command": "kept=yes"})),
            say("set"),
            call("2", "bash", &json!({"command": "echo $kept"})),
            say("gone"),
            call("3", "bash", &json!({"command": "echo \"[$kept]\""})),
            say("back, but empty"),
        ],
    )
    .unwrap();

    agent.harness.run("set a variable").await.unwrap();
    agent.harness.app().dispose(agent.kernel).unwrap();
    agent.harness.run("read it").await.unwrap();
    agent.harness.plugin(shell_kernel("shell", &work));
    agent.harness.run("read it again").await.unwrap();

    assert_eq!(
        outputs(&agent.harness),
        ["", "tool failed: unknown kernel `shell`", "[]\n"]
    );
}
