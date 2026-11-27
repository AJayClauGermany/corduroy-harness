//! The `viewer` add-on through the loop with a scripted model: the log written as a page
//! of files, kept up from one process to the next, and written again without a session
//! that was removed from the log. Build the add-ons first with `cargo plugins`. No network,
//! browser or LLM: what the page would load is read as the files it is.

use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use corduroy_harness::core::{EventStream, LogError, Message, Pattern, Scope, SessionId};
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::wasm::{WasmPlugin, WasmRuntime};
use corduroy_harness::{Harness, removal};
use corduroy_jsonl as jsonl;
use corduroy_test_support::{plugin, scratch};
use serde_json::{Value, json};

/// The plugin `name`, with every capability it imports linked but empty, then `grants`.
fn load(work: &Path, name: &str, grants: &[(&str, Scope)]) -> Result<WasmPlugin, Box<dyn Error>> {
    let mut loaded = WasmRuntime::new()?.with_root(work).load(plugin(name))?;
    for capability in loaded.requires().to_vec() {
        loaded = loaded.grant(capability, Scope::none());
    }
    for (capability, scope) in grants {
        loaded = loaded.grant(*capability, scope.clone());
    }
    Ok(loaded)
}

fn within(dir: &Path) -> Scope {
    Scope::subtree(&dir.to_string_lossy())
}

/// The viewer, writing into `dir` everything in the log.
fn viewer(work: &Path, dir: &Path) -> Result<WasmPlugin, Box<dyn Error>> {
    viewer_of(work, dir, Scope::any())
}

/// The viewer, writing into `dir` what `reads` reaches of the log.
fn viewer_of(work: &Path, dir: &Path, reads: Scope) -> Result<WasmPlugin, Box<dyn Error>> {
    let scope = within(dir);
    let grants = [
        ("log-read", reads),
        ("fs-read", scope.clone()),
        ("fs-write", scope),
        ("log", Scope::any()),
    ];
    Ok(load(work, "viewer", &grants)?.config(json!({"dir": dir}).to_string()))
}

/// The `find` tool, for runs that call one.
fn search(work: &Path) -> Result<WasmPlugin, Box<dyn Error>> {
    load(work, "search", &[("fs-read", within(work))])
}

/// A harness over the log at `path`, or one kept in memory, whose model gives `replies`.
fn harness(
    path: Option<&Path>,
    replies: impl IntoIterator<Item = Message>,
) -> Result<Harness, Box<dyn Error>> {
    let events: EventStream = match path {
        Some(path) => jsonl::open(path)?,
        None => EventStream::new(),
    };
    let model = Arc::new(ScriptedProvider::new(replies));
    Ok(Harness::resume(model, Arc::new(events))?)
}

/// `count` calls of `find`, numbered from `from`, then an answer.
fn finds(from: usize, count: usize) -> impl Iterator<Item = Message> {
    (from..from + count)
        .map(|at| call(&at.to_string(), "find", &json!({"pattern": "*.rs"})))
        .chain([say("done")])
}

/// What the page's index says.
fn index(dir: &Path) -> Result<Value, Box<dyn Error>> {
    let script = fs::read_to_string(dir.join("index.js"))?;
    let json = script
        .trim()
        .strip_prefix("corduroyIndex(")
        .and_then(|rest| rest.strip_suffix(");"))
        .ok_or("index.js is not an index")?;
    Ok(serde_json::from_str(json)?)
}

/// Every event the page would read, in the order it reads them: each chunk the index
/// names, as the JSON array its script holds.
fn shown(dir: &Path) -> Result<Vec<Value>, Box<dyn Error>> {
    let chunks = index(dir)?["chunks"].as_u64().ok_or("no chunk count")?;
    let mut events = Vec::new();
    for number in 0..chunks {
        let script = fs::read_to_string(dir.join(format!("events-{number:06}.js")))?;
        let array = script
            .strip_prefix(&format!("corduroyChunk({number},"))
            .and_then(|rest| rest.strip_suffix(");\n"))
            .ok_or("not a chunk")?
            // The last event ends in a comma, which JavaScript takes and JSON does not.
            .replace(",\n]", "\n]");
        let Value::Array(chunk) = serde_json::from_str(&array)? else {
            return Err("a chunk is not an array".into());
        };
        events.extend(chunk);
    }
    Ok(events)
}

/// The ids of the events the page would read.
fn shown_ids(dir: &Path) -> Result<Vec<u64>, Box<dyn Error>> {
    Ok(shown(dir)?
        .iter()
        .filter_map(|event| event["id"].as_u64())
        .collect())
}

/// The ids of the events on the stream.
fn stream_ids(harness: &Harness) -> Vec<u64> {
    harness
        .events()
        .events()
        .iter()
        .map(|event| event.id.get())
        .collect()
}

/// Whether any file in `dir` holds `word`.
fn holds(dir: &Path, word: &str) -> Result<bool, Box<dyn Error>> {
    for entry in fs::read_dir(dir)? {
        if fs::read_to_string(entry?.path())?.contains(word) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Take `session` out of the log at `path`, as `corduroy sessions remove` does.
fn remove(path: &Path, session: &str) -> Result<(), Box<dyn Error>> {
    let session = SessionId::new(session);
    jsonl::remove(path, |events| {
        let removal =
            removal(events, &session).map_err(|error| LogError::new(error.to_string()))?;
        Ok((removal.sessions, removal.markers))
    })?;
    Ok(())
}

#[tokio::test]
async fn the_viewer_writes_the_log_as_a_page() {
    let work = scratch("viewer-page");
    fs::write(work.join("lib.rs"), "pub fn add() {}\n").unwrap();
    let dir = work.join(".corduroy/viewer");
    let replies = [
        call("1", "find", &json!({"pattern": "*.rs"})),
        call("2", "grep", &json!({"pattern": "(unclosed"})),
        say("one Rust file"),
        say("still one"),
    ];
    let harness = harness(None, replies).unwrap();
    harness.plugin(search(&work).unwrap());
    harness.plugin(viewer(&work, &dir).unwrap());

    harness.run("what is here?").await.unwrap();
    harness.run("and now?").await.unwrap();

    for file in ["index.html", "app.js", "style.css", "index.js"] {
        assert!(dir.join(file).is_file(), "{file} was not written");
    }
    // Everything on the stream is on the page, each event once and in order.
    let page = shown(&dir).unwrap();
    assert_eq!(shown_ids(&dir).unwrap(), stream_ids(&harness));
    let written = index(&dir).unwrap();
    assert_eq!(written["generation"], json!(1));
    assert_eq!(written["behind"], json!(false));
    assert_eq!(written["sessions"]["main"], written["last"]);
    let prompt = page
        .iter()
        .find(|event| event["topic"] == "message" && event["source"] == "user")
        .unwrap();
    assert_eq!(
        prompt["payload"]["parts"][0]["text"],
        json!("what is here?")
    );
    assert_eq!(prompt["session"], json!("main"));
    assert!(
        page.iter()
            .any(|event| event["payload"]["ok"] == json!(false))
    );
    // It writes, and says nothing: where a page is, is the agent file's to say.
    assert_eq!(harness.events().matching(&Pattern::new("plugin/log")), []);
}

#[tokio::test]
async fn a_new_instance_goes_on_from_what_the_last_one_wrote() {
    let work = scratch("viewer-resume");
    let dir = work.join("page");
    let harness = harness(None, finds(0, 3).chain(finds(3, 3))).unwrap();
    harness.plugin(search(&work).unwrap());
    let first = harness.plugin(viewer(&work, &dir).unwrap());
    harness.run("look").await.unwrap();
    let before = shown_ids(&dir).unwrap();

    // As after a restart: what the instance held in memory is gone.
    harness.app().dispose(first).unwrap();
    harness.plugin(viewer(&work, &dir).unwrap());
    harness.run("look again").await.unwrap();

    let after = shown_ids(&dir).unwrap();
    assert_eq!(after, stream_ids(&harness));
    assert!(after.len() > before.len() && after.starts_with(&before));
    let written = index(&dir).unwrap();
    assert_eq!(written["generation"], json!(1));
    assert_eq!(written["events"], json!(after.len()));
}

#[tokio::test]
async fn a_long_log_is_written_in_chunks() {
    let work = scratch("viewer-chunks");
    let dir = work.join("page");
    let harness = harness(None, finds(0, 20).chain(finds(20, 20)).chain(finds(40, 20))).unwrap();
    harness.plugin(search(&work).unwrap());
    harness.plugin(viewer(&work, &dir).unwrap());

    for _ in 0..3 {
        harness.run("count the files").await.unwrap();
    }

    let written = index(&dir).unwrap();
    assert!(written["chunks"].as_u64().unwrap() >= 3, "{written}");
    assert_eq!(shown_ids(&dir).unwrap(), stream_ids(&harness));
    // Every chunk but the last is full, and none is over.
    let first = fs::read_to_string(dir.join("events-000000.js")).unwrap();
    assert_eq!(first.lines().count(), 200 + 2);
}

#[tokio::test]
async fn a_removed_session_leaves_the_page_and_the_files_it_filled() {
    let work = scratch("viewer-removed");
    let (dir, path) = (work.join("page"), work.join("log.jsonl"));
    {
        // Enough in the session that goes to fill a chunk the page then no longer needs.
        let replies = finds(0, 2).chain(finds(2, 20)).chain(finds(22, 20));
        let harness = harness(Some(&path), replies).unwrap();
        let tools = harness.plugin(search(&work).unwrap());
        let page = harness.plugin(viewer(&work, &dir).unwrap());
        harness.run("count the files").await.unwrap();
        let side = harness.start_session("side", None).unwrap();
        for _ in 0..2 {
            harness
                .run_in(&side, "the launch code is 4471")
                .await
                .unwrap();
        }
        // An add-on holds the stream, and so the log's lock, until it is unloaded.
        harness.app().dispose(page).unwrap();
        harness.app().dispose(tools).unwrap();
    }
    let before = index(&dir).unwrap();
    assert!(before["chunks"].as_u64().unwrap() >= 2, "{before}");
    assert!(holds(&dir, "4471").unwrap());

    remove(&path, "side").unwrap();
    let harness = harness(Some(&path), []).unwrap();
    harness.plugin(viewer(&work, &dir).unwrap());

    // Written again from the start, without the session: no event of it, no file left
    // over from when it was there, and its words nowhere in the directory.
    let after = index(&dir).unwrap();
    assert_eq!(after["generation"], json!(2));
    assert_eq!(after["chunks"], json!(1));
    let page = shown(&dir).unwrap();
    assert!(page.iter().all(|event| event["session"] != json!("side")));
    assert!(
        page.iter()
            .any(|event| event["topic"] == json!("session/removed"))
    );
    // All of the log but the viewer's own start, which is recorded once it has started.
    let stream = stream_ids(&harness);
    assert_eq!(shown_ids(&dir).unwrap(), stream[..stream.len() - 1]);
    let files: BTreeSet<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let expected = [
        "app.js",
        "events-000000.js",
        "index.html",
        "index.js",
        "style.css",
    ];
    assert_eq!(files, expected.map(str::to_owned).into());
    assert!(!holds(&dir, "4471").unwrap());
}

#[tokio::test]
async fn a_session_out_of_the_log_or_out_of_the_grant_leaves_the_page() {
    let work = scratch("viewer-narrow");
    let (dir, path) = (work.join("page"), work.join("log.jsonl"));
    // Granted the two sessions and nothing about the harness: no removal marker reaches it.
    let sessions = || Scope::none().allow("main:*").allow("side:*");
    {
        let harness = harness(Some(&path), [say("noted"), say("kept")]).unwrap();
        let page = harness.plugin(viewer_of(&work, &dir, sessions()).unwrap());
        harness.run("the door code is 1234").await.unwrap();
        let side = harness.start_session("side", None).unwrap();
        harness
            .run_in(&side, "the launch code is 4471")
            .await
            .unwrap();
        harness.app().dispose(page).unwrap();
    }
    assert!(holds(&dir, "1234").unwrap() && holds(&dir, "4471").unwrap());

    remove(&path, "side").unwrap();
    {
        let harness = harness(Some(&path), []).unwrap();
        let page = harness.plugin(viewer_of(&work, &dir, sessions()).unwrap());
        assert_eq!(index(&dir).unwrap()["generation"], json!(2));
        assert!(holds(&dir, "1234").unwrap() && !holds(&dir, "4471").unwrap());
        harness.app().dispose(page).unwrap();
    }

    // The grant is narrowed to nothing of `main`: what the page held of it goes too.
    let harness = harness(Some(&path), []).unwrap();
    harness.plugin(viewer_of(&work, &dir, Scope::none().allow("side:*")).unwrap());
    let written = index(&dir).unwrap();
    assert_eq!(written["generation"], json!(3));
    assert_eq!(written["events"], json!(0));
    assert!(!holds(&dir, "1234").unwrap());
}
