//! One add-on in another's place: `lean-shell` offering `bash` instead of `pi-tools`, through
//! the loop with a scripted model and a real shell. The same conversation is run against
//! each, and the model, the gates and the log are shown not to tell them apart.
//! Build the add-ons first with `cargo plugins`. No network or LLM.
#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;

use corduroy_harness::core::{Message, Scope, SessionId};
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::wasm::WasmRuntime;
use corduroy_harness::{Answer, Harness, MAIN, Status};
use corduroy_shell::shell_kernel;
use corduroy_test_support::{plugin, scratch};
use serde_json::{Value, json};

/// Which add-on offers `bash`.
#[derive(Clone, Copy)]
enum Shell {
    /// `pi-tools`, as shipped.
    Shipped,
    /// `lean-shell`, with `pi-tools` configured without `bash`.
    Replaced,
    /// Both, neither giving way.
    Both,
}

/// A workspace and, beside it, the program `lean-shell` puts commands through.
struct Place {
    work: PathBuf,
    /// The program: asked to `rewrite` a script, it notes the script in `asked` and
    /// answers with the same script, as `rtk rewrite` does for one it would not change.
    program: PathBuf,
    /// One line for each script the program was asked about.
    asked: PathBuf,
}

fn place(test: &str) -> Result<Place, Box<dyn Error>> {
    let dir = scratch(test);
    let work = dir.join("acme-site");
    fs::create_dir_all(work.join("sub"))?;
    let program = dir.join("shorten");
    let asked = dir.join("asked");
    fs::write(
        &program,
        format!(
            "#!/bin/sh\n[ \"$1\" = rewrite ] || exit 1\nprintf '%s\\n' \"$2\" >> '{}'\n\
             printf '%s\\n' \"$2\"\n",
            asked.display()
        ),
    )?;
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755))?;
    Ok(Place {
        work,
        program,
        asked,
    })
}

/// A harness in `place` with a shell kernel and `bash` from `shell`, asking `replies`.
fn agent(
    place: &Place,
    shell: Shell,
    replies: impl IntoIterator<Item = Message>,
) -> Result<Harness, Box<dyn Error>> {
    let harness = Harness::new(Arc::new(ScriptedProvider::new(replies)))?;
    let runtime = WasmRuntime::new()?.with_root(&place.work);
    let workspace = Scope::subtree(&place.work.to_string_lossy());
    let kernel = Scope::none().allow("shell");
    harness.plugin(shell_kernel("shell", &place.work));
    let files = runtime
        .load(plugin("pi-tools"))?
        .grant("fs-read", workspace.clone())
        .grant("fs-write", workspace)
        .grant("kernel", kernel.clone());
    harness.plugin(match shell {
        Shell::Shipped | Shell::Both => files,
        Shell::Replaced => {
            files.config(json!({"tools": ["read", "write", "edit", "tree"]}).to_string())
        }
    });
    if matches!(shell, Shell::Replaced | Shell::Both) {
        harness.plugin(
            runtime
                .load(plugin("lean-shell"))?
                .grant("kernel", kernel)
                .config(json!({"prefix": place.program}).to_string()),
        );
    }
    Ok(harness)
}

/// The events on `topics` so far, each as its topic and payload.
fn trail(harness: &Harness, topics: &[&str]) -> Vec<(String, Value)> {
    harness
        .events()
        .events()
        .into_iter()
        .filter(|event| topics.contains(&event.topic.as_str()))
        .map(|event| {
            let payload = serde_json::from_str(&event.payload).unwrap_or(Value::Null);
            (event.topic, payload)
        })
        .collect()
}

/// The output of every tool call in `trail`, in order.
fn outputs_of(trail: &[(String, Value)]) -> Vec<String> {
    trail
        .iter()
        .filter(|(topic, _)| topic == "tool/result")
        .filter_map(|(_, result)| Some(result.get("output")?.as_str()?.to_owned()))
        .collect()
}

/// A conversation that leans on what `bash` promises: state that carries over, a status
/// that is not 0, several lines, both kinds of quote, and a file made in the workspace.
fn conversation() -> Vec<Message> {
    vec![
        call("1", "bash", &json!({"command": "kept=yes; cd sub"})),
        call(
            "2",
            "bash",
            &json!({"command": "echo \"$kept in $(basename \"$PWD\")\""}),
        ),
        call("3", "bash", &json!({"command": "(echo oops; exit 3)"})),
        call(
            "4",
            "bash",
            &json!({"command": "printf '%s\\n' 'a b'\nprintf '%s\\n' \"it's {here}\" > note.txt\ncat note.txt", "timeout": 30}),
        ),
        call("5", "read", &json!({"path": "sub/note.txt"})),
        say("done"),
    ]
}

/// What the model, the gates and a reader of the log see of a run: everything but which
/// plugins were activated.
const SEEN: [&str; 8] = [
    "message",
    "model/request",
    "tool/call",
    "tool/start",
    "tool/result",
    "gate/decision",
    "gate/answer",
    "turn/end",
];

#[tokio::test]
async fn a_replacement_leaves_the_trail_its_original_would() {
    let mut trails = Vec::new();
    for shell in [Shell::Shipped, Shell::Replaced] {
        let place = place("replacement-trail").unwrap();
        let harness = agent(&place, shell, conversation()).unwrap();

        let outcome = harness.run("go").await.unwrap();

        assert_eq!(
            outcome.status,
            Status::Finished {
                answer: "done".into()
            }
        );
        assert_eq!(
            fs::read_to_string(place.work.join("sub/note.txt")).unwrap(),
            "it's {here}\n"
        );
        // Only the replacement went through the program, once for each command, with the
        // command as the model wrote it.
        let asked = fs::read_to_string(&place.asked).unwrap_or_default();
        match shell {
            Shell::Replaced => assert_eq!(
                asked,
                "kept=yes; cd sub\necho \"$kept in $(basename \"$PWD\")\"\n\
                 (echo oops; exit 3)\nprintf '%s\\n' 'a b'\n\
                 printf '%s\\n' \"it's {here}\" > note.txt\ncat note.txt\n"
            ),
            Shell::Shipped | Shell::Both => assert_eq!(asked, ""),
        }
        trails.push(trail(&harness, &SEEN));
    }

    let (shipped, replaced) = (&trails[0], &trails[1]);
    assert_eq!(
        outputs_of(shipped),
        [
            "",
            "yes in sub\n",
            "oops\n[exit status 3]",
            "a b\nit's {here}\n",
            "it's {here}"
        ]
    );
    // Not one event differs: the same five tools offered, the same calls started, the
    // same results recorded.
    assert_eq!(shipped, replaced);
}

#[tokio::test]
async fn a_gate_asks_and_refuses_a_replacement_as_it_would_the_original() {
    let mut trails = Vec::new();
    for shell in [Shell::Shipped, Shell::Replaced] {
        let place = place("replacement-gate").unwrap();
        let harness = agent(
            &place,
            shell,
            [
                call("1", "bash", &json!({"command": "echo made > made.txt"})),
                call("2", "bash", &json!({"command": "rm -rf sub"})),
                say("done"),
            ],
        )
        .unwrap();
        harness.plugin(
            WasmRuntime::new()
                .unwrap()
                .load(plugin("approval"))
                .unwrap()
                .config(json!({"mode": "smart"}).to_string()),
        );

        let first = harness.run("go").await.unwrap();

        // The gate's rule is for a tool called `bash`, whoever offers it.
        let Status::Waiting { asks, .. } = first.status else {
            panic!("expected to wait for the command, got {:?}", first.status);
        };
        assert_eq!(
            (asks[0].tool.as_str(), asks[0].gate.as_str()),
            ("bash", "approval")
        );
        assert!(!place.work.join("made.txt").exists());
        let main = SessionId::new(MAIN);
        let second = harness.answer(&main, "1", Answer::Allow).await.unwrap();
        assert!(
            matches!(second.status, Status::Waiting { .. }),
            "{second:?}"
        );
        let last = harness
            .answer(&main, "2", Answer::Deny("keep it".into()))
            .await
            .unwrap();

        assert_eq!(
            last.status,
            Status::Finished {
                answer: "done".into()
            }
        );
        assert_eq!(
            fs::read_to_string(place.work.join("made.txt")).unwrap(),
            "made\n"
        );
        assert!(place.work.join("sub").is_dir());
        trails.push(trail(&harness, &SEEN));
    }

    assert!(trails[0].iter().any(|(topic, _)| topic == "gate/decision"));
    assert_eq!(trails[0], trails[1]);
}

#[tokio::test]
async fn the_second_add_on_to_offer_a_tool_is_refused_whole() {
    let place = place("replacement-both").unwrap();
    let harness = agent(
        &place,
        Shell::Both,
        [
            call("1", "bash", &json!({"command": "echo hi"})),
            say("done"),
        ],
    )
    .unwrap();

    harness.run("go").await.unwrap();

    let failed = trail(&harness, &["plugin/failed"]);
    assert_eq!(
        failed,
        [(
            "plugin/failed".to_owned(),
            json!({"plugin": "lean_shell", "reason": "`bash` is already registered"})
        )]
    );
    // The first keeps the name: the command ran, and not through the program.
    assert_eq!(outputs_of(&trail(&harness, &SEEN)), ["hi\n"]);
    assert!(!place.asked.exists());
}

#[tokio::test]
async fn a_tool_left_to_another_add_on_is_not_offered_and_an_unknown_one_is_refused() {
    let place = place("replacement-without").unwrap();
    let harness = Harness::new(Arc::new(ScriptedProvider::new([say("done")]))).unwrap();
    let runtime = WasmRuntime::new().unwrap().with_root(&place.work);
    let load = |config: Value| {
        runtime
            .load(plugin("pi-tools"))
            .unwrap()
            .grant("fs-read", Scope::none())
            .grant("fs-write", Scope::none())
            .grant("kernel", Scope::none())
            .config(config.to_string())
    };
    harness.plugin(load(json!({"tools": ["read", "tree"]})).named("some"));
    harness.plugin(load(json!({"tools": ["read", "grep"]})).named("wrong"));

    harness.run("go").await.unwrap();

    let offered: Vec<String> = harness
        .tools()
        .specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect();
    assert_eq!(offered, ["read", "tree"]);
    let failed = trail(&harness, &["plugin/failed"]);
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0].1["plugin"], "wrong");
    assert_eq!(
        failed[0].1["reason"],
        "plugin `wrong` refused to start: bad config: `tools` names `grep`, and the tools are read, write, edit, bash, tree"
    );
}

#[tokio::test]
async fn a_program_that_is_not_installed_fails_the_call_and_says_so() {
    let mut place = place("replacement-absent").unwrap();
    place.program = PathBuf::from("no-such-shortener");
    let harness = agent(
        &place,
        Shell::Replaced,
        [
            call("1", "bash", &json!({"command": "echo made > made.txt"})),
            say("done"),
        ],
    )
    .unwrap();

    harness.run("go").await.unwrap();

    let results = trail(&harness, &["tool/result"]);
    assert_eq!(results[0].1["ok"], false);
    assert_eq!(
        results[0].1["output"],
        "tool failed: `no-such-shortener` is not installed where the shell finds commands (its PATH), so \
         the command was not run. This tool puts every command through it; it is set as \
         `prefix` in this add-on's configuration."
    );
    // It is not run another way.
    assert!(!place.work.join("made.txt").exists());
}
