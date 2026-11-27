//! One add-on in another's place: `lean-shell` offering `bash` instead of `pi-tools`, through
//! the loop with a scripted model and a real shell. The same conversation is run against
//! each, and the model, the gates and the log are shown not to tell them apart. The
//! replacement is said (`replacing`) and checked: a tool that could not be taken for the
//! one it replaces is refused, and the name goes back when the replacement is unloaded.
//! Build the add-ons first with `cargo plugins`. No network or LLM.
#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;

use corduroy_harness::core::{
    FiberId, Message, Scope, SessionId, TOOLS, ToolSpec, Tools, plugin_fn, tool_fn,
};
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
    /// `lean-shell`, said to replace `pi-tools`' `bash`.
    Replaced,
    /// `lean-shell`, with `pi-tools` configured without `bash`: nothing to replace.
    Left,
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
    Ok(agent_with(place, shell, replies)?.0)
}

/// A harness, and what `lean-shell` was loaded as in it, if it was.
type Loaded = (Harness, Option<FiberId>);

/// The same, with what `lean-shell` was loaded as, if it was.
fn agent_with(
    place: &Place,
    shell: Shell,
    replies: impl IntoIterator<Item = Message>,
) -> Result<Loaded, Box<dyn Error>> {
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
        Shell::Shipped | Shell::Replaced | Shell::Both => files,
        Shell::Left => {
            files.config(json!({"tools": ["read", "write", "edit", "tree"]}).to_string())
        }
    });
    if matches!(shell, Shell::Shipped) {
        return Ok((harness, None));
    }
    let lean = runtime
        .load(plugin("lean-shell"))?
        .grant("kernel", kernel)
        .config(json!({"prefix": place.program}).to_string());
    let lean = harness.plugin(match shell {
        Shell::Replaced => lean.replacing(["bash"]),
        _ => lean,
    });
    Ok((harness, Some(lean)))
}

/// `trail` without what an add-on in another's place may say differently: its own words
/// for the tool, which the request names by hash and counts by size, and how much of a
/// result it keeps.
fn alike(trail: &[(String, Value)]) -> Vec<(String, Value)> {
    let mut trail = trail.to_vec();
    for (topic, payload) in &mut trail {
        let Some(fields) = payload.as_object_mut() else {
            continue;
        };
        let own: &[&str] = match topic.as_str() {
            "model/request" => &["tools_hash", "estimated_tokens"],
            "tool/result" => &["budget", "cut"],
            _ => &[],
        };
        for field in own {
            fields.remove(*field);
        }
    }
    trail
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
    for shell in [Shell::Shipped, Shell::Replaced, Shell::Left] {
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
            Shell::Replaced | Shell::Left => assert_eq!(
                asked,
                "kept=yes; cd sub\necho \"$kept in $(basename \"$PWD\")\"\n\
                 (echo oops; exit 3)\nprintf '%s\\n' 'a b'\n\
                 printf '%s\\n' \"it's {here}\" > note.txt\ncat note.txt\n"
            ),
            Shell::Shipped | Shell::Both => assert_eq!(asked, ""),
        }
        trails.push(trail(&harness, &SEEN));
    }

    let (shipped, replaced, left) = (&trails[0], &trails[1], &trails[2]);
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
    // Not one event differs but in what is the add-on's own to say: the same five tools
    // offered, the same calls started, the same results recorded.
    assert_eq!(alike(shipped), alike(replaced));
    // And it is the same whether the name was taken over or left free for it.
    assert_eq!(replaced, left);
    // What differs is said in the log: the two were shown as different sets of tools.
    assert_ne!(shipped[1].1["tools_hash"], replaced[1].1["tools_hash"]);
}

/// The names of the tools on offer, and which of them stand in for another's.
fn offered(harness: &Harness) -> Vec<(String, bool)> {
    harness
        .tools()
        .specs()
        .into_iter()
        .map(|spec| {
            let replaced = harness.tools().replaced(&spec.name);
            (spec.name, replaced)
        })
        .collect()
}

#[tokio::test]
async fn a_replacement_gives_the_name_back_when_it_is_unloaded() {
    let place = place("replacement-back").unwrap();
    let (harness, lean) = agent_with(
        &place,
        Shell::Replaced,
        [
            call("1", "bash", &json!({"command": "echo one"})),
            say("done"),
            call("2", "bash", &json!({"command": "echo two"})),
            say("done"),
        ],
    )
    .unwrap();

    harness.run("go").await.unwrap();
    let names = |replaced: bool| {
        ["bash", "edit", "read", "tree", "write"]
            .map(|name| (name.to_owned(), replaced && name == "bash"))
    };
    assert_eq!(offered(&harness), names(true));
    assert_eq!(fs::read_to_string(&place.asked).unwrap(), "echo one\n");

    // Without it, `bash` is the one it stood in for, which was there all along.
    harness.app().dispose(lean.unwrap()).unwrap();
    assert_eq!(offered(&harness), names(false));
    harness.run("again").await.unwrap();
    assert_eq!(fs::read_to_string(&place.asked).unwrap(), "echo one\n");
    assert_eq!(
        outputs_of(&trail(&harness, &["tool/result"])),
        ["one\n", "two\n"]
    );
}

/// Each plugin that did not start, with why.
fn failures(harness: &Harness) -> Vec<(String, String)> {
    trail(harness, &["plugin/failed"])
        .into_iter()
        .map(|(_, failed)| {
            let text = |key: &str| failed[key].as_str().unwrap_or_default().to_owned();
            (text("plugin"), text("reason"))
        })
        .collect()
}

#[tokio::test]
async fn a_tool_that_cannot_be_taken_for_the_one_it_replaces_is_refused() {
    let place = place("replacement-differs").unwrap();
    let harness = agent(&place, Shell::Shipped, [say("done")]).unwrap();
    // One that takes something else, and one that says a call may be made again.
    let stand_in = |name: &'static str, spec: ToolSpec| {
        plugin_fn(name, &[TOOLS], move |ctx| {
            let tools = ctx.service::<Tools>(TOOLS)?;
            tools.stand_in(ctx, Arc::new(tool_fn(spec.clone(), |_| Ok("mine"))))?;
            Ok(())
        })
    };
    let used = harness.tools().get("bash").unwrap().spec();
    let other = ToolSpec::new("bash", "Runs it.", r#"{"type":"object","properties":{}}"#);
    let repeated = ToolSpec::new("bash", "Runs it.", used.input_schema.clone()).replay_safe(true);
    let nothing = ToolSpec::new("zsh", "Runs it.", used.input_schema.clone());
    harness.plugin(stand_in("other-schema", other));
    harness.plugin(stand_in("other-word", repeated));
    harness.plugin(stand_in("nothing-there", nothing));
    // An add-on told to replace a tool it does not offer, and one with nothing to replace.
    let runtime = WasmRuntime::new().unwrap().with_root(&place.work);
    let lean = || {
        runtime
            .load(plugin("lean-shell"))
            .unwrap()
            .grant("kernel", Scope::none().allow("shell"))
    };
    harness.plugin(lean().named("no-such-tool").replacing(["bash", "read"]));

    harness.run("go").await.unwrap();

    assert_eq!(
        failures(&harness),
        [
            (
                "other-schema".to_owned(),
                "`bash` cannot be replaced by this one: its input schema is not the same as \
                 that of the tool it replaces"
                    .to_owned()
            ),
            (
                "other-word".to_owned(),
                "`bash` cannot be replaced by this one: it says a call may be made again \
                 after a crash (replay-safe), and the tool it replaces does not"
                    .to_owned()
            ),
            (
                "nothing-there".to_owned(),
                "`zsh` is not registered, so it cannot be replaced: what offers it comes first"
                    .to_owned()
            ),
            (
                "no-such-tool".to_owned(),
                "plugin `no-such-tool` is to replace the tool `read`, and offers no tool of \
                 that name"
                    .to_owned()
            ),
        ]
    );
    // Nothing was taken over, not even by the add-on whose `bash` would have passed.
    assert!(offered(&harness).iter().all(|(_, replaced)| !replaced));
    assert_eq!(harness.tools().get("bash").unwrap().spec(), used);
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
    assert_eq!(alike(&trails[0]), alike(&trails[1]));
}

#[tokio::test]
async fn the_second_add_on_to_offer_a_tool_without_saying_it_replaces_it_is_refused_whole() {
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
