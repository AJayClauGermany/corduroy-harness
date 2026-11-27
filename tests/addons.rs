//! The add-ons for working in a real workspace, each through the loop with a scripted
//! model: `search` (`grep`, `find`), `fetch` and `today`. `tests/viewer.rs` has the fourth.
//! Build them first with `cargo plugins`. No network or LLM.

use std::error::Error;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use corduroy_harness::Harness;
use corduroy_harness::core::{HttpClient, HttpRequest, HttpResponse, Message, Pattern, Scope};
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::wasm::{WasmPlugin, WasmRuntime};
use corduroy_test_support::{plugin, scratch};
use serde_json::{Value, json};

/// A harness and the scripted model it asks.
struct Agent {
    harness: Harness,
    model: Arc<ScriptedProvider>,
}

/// A harness with the scripted `replies`.
fn agent(replies: impl IntoIterator<Item = Message>) -> Result<Agent, Box<dyn Error>> {
    let model = Arc::new(ScriptedProvider::new(replies));
    let harness = Harness::new(Arc::<ScriptedProvider>::clone(&model))?;
    Ok(Agent { harness, model })
}

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

fn within(work: &Path) -> Scope {
    Scope::subtree(&work.to_string_lossy())
}

/// A small project: two Rust files, a note, and directories a search passes over.
fn project(work: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(work.join("src/net"))?;
    fs::create_dir_all(work.join("target/debug"))?;
    fs::create_dir_all(work.join(".git"))?;
    fs::write(
        work.join("src/lib.rs"),
        "pub mod net;\n\n/// Adds.\npub fn add(a: u8, b: u8) -> u8 {\n    a + b\n}\n",
    )?;
    fs::write(
        work.join("src/net/client.rs"),
        "pub fn connect() {}\n// TODO: retry\npub fn send() {}\n",
    )?;
    fs::write(work.join("NOTES.md"), "todo: write the docs\n")?;
    fs::write(
        work.join("target/debug/build.rs"),
        "pub fn generated() {}\n",
    )?;
    fs::write(work.join(".git/config"), "pub fn not_source() {}\n")?;
    fs::write(work.join("logo.png"), [0xff, 0xd8, 0xff, 0x00])?;
    Ok(())
}

#[tokio::test]
async fn grep_finds_lines_by_regular_expression_without_a_shell() {
    let work = scratch("addons-grep");
    project(&work).unwrap();
    let Agent {
        harness,
        model: _model,
    } = agent([
        call("1", "grep", &json!({"pattern": r"pub fn \w+\("})),
        call(
            "2",
            "grep",
            &json!({"pattern": "todo", "ignore_case": true, "context": 1, "glob": "*.rs"}),
        ),
        call(
            "3",
            "grep",
            &json!({"pattern": "todo", "ignore_case": true, "files_only": true}),
        ),
        call("4", "grep", &json!({"pattern": "fn", "path": "src/lib.rs"})),
        call("5", "grep", &json!({"pattern": "pub fn", "limit": 1})),
        call("6", "grep", &json!({"pattern": "nowhere"})),
        call("7", "grep", &json!({"pattern": "(unclosed"})),
        call("8", "grep", &json!({"pattern": "x", "path": "missing"})),
        say("searched"),
    ])
    .unwrap();
    harness.plugin(load(&work, "search", &[("fs-read", within(&work))]).unwrap());

    harness.run("find the functions").await.unwrap();

    let said = outputs(&harness);
    // Files of a directory come before its directories; `target` and `.git` are not opened.
    assert_eq!(
        said[0],
        "src/lib.rs:4:pub fn add(a: u8, b: u8) -> u8 {\n\
         src/net/client.rs:1:pub fn connect() {}\n\
         src/net/client.rs:3:pub fn send() {}"
    );
    assert_eq!(
        said[1],
        "src/net/client.rs-1-pub fn connect() {}\n\
         src/net/client.rs:2:// TODO: retry\n\
         src/net/client.rs-3-pub fn send() {}"
    );
    assert_eq!(said[2], "NOTES.md\nsrc/net/client.rs");
    assert_eq!(said[3], "src/lib.rs:4:pub fn add(a: u8, b: u8) -> u8 {");
    assert!(
        said[4].ends_with(
            "[stopped at 1 matching lines: narrow the pattern or the path, or raise `limit`]"
        ),
        "{}",
        said[4]
    );
    assert_eq!(
        said[5],
        "Nothing matches `nowhere` in the 3 file(s) read under `.`."
    );
    assert!(said[6].contains("`pattern`:"), "{}", said[6]);
    assert!(said[7].contains("missing"), "{}", said[7]);
    // Both are offered as tools that only look.
    let specs = harness.tools().specs();
    let grep = specs.iter().find(|spec| spec.name == "grep").unwrap();
    assert!(grep.read_only && grep.replay_safe);
}

#[tokio::test]
async fn find_lists_files_by_a_pattern_on_their_path() {
    let work = scratch("addons-find");
    project(&work).unwrap();
    let Agent {
        harness,
        model: _model,
    } = agent([
        call("1", "find", &json!({"pattern": "*.rs"})),
        call("2", "find", &json!({"pattern": "src/*.rs"})),
        call("3", "find", &json!({"pattern": "*.{md,png}"})),
        call("4", "find", &json!({"pattern": "*.rs", "path": "src/net"})),
        call("5", "find", &json!({"pattern": "*.rs", "limit": 1})),
        call("6", "find", &json!({"pattern": "*.py"})),
        call("7", "find", &json!({"pattern": "*", "path": "../outside"})),
        say("found"),
    ])
    .unwrap();
    harness.plugin(load(&work, "search", &[("fs-read", within(&work))]).unwrap());

    harness.run("what is here?").await.unwrap();

    let said = outputs(&harness);
    assert_eq!(said[0], "src/lib.rs\nsrc/net/client.rs");
    assert_eq!(said[1], "src/lib.rs");
    assert_eq!(said[2], "NOTES.md\nlogo.png");
    assert_eq!(said[3], "src/net/client.rs");
    assert!(
        said[4].starts_with("src/lib.rs\n[the first 1 are shown"),
        "{}",
        said[4]
    );
    assert_eq!(said[5], "No file under `.` matches `*.py` (4 looked at).");
    // Outside the grant: refused, and said.
    assert!(said[6].contains("outside"), "{}", said[6]);
}

/// What the fake web answers for `url`: a status, a content type or a place to go, a body.
fn page(url: &str) -> (u16, &'static str, String) {
    let html = "<html><head><title>Guide</title><script>track()</script></head><body>\
                <h1>Install</h1><p>Run <code>make</code>, then see <a href=\"api.html\">the \
                API</a>.</p></body></html>";
    match url {
        "https://docs.example/guide" => (200, "text/html; charset=utf-8", html.to_owned()),
        "https://docs.example/old" => (301, "/guide", String::new()),
        "https://docs.example/away" => (302, "https://elsewhere.example/x", String::new()),
        "https://docs.example/loop" => (302, "/loop", String::new()),
        "https://docs.example/data.json" => (200, "application/json", "{\"ok\":true}".to_owned()),
        "https://docs.example/logo.png" => (200, "image/png", "\u{89}PNG".to_owned()),
        "https://docs.example/long" => (200, "text/plain", "0123456789".repeat(30)),
        _ => (404, "text/html", "<h1>Not here</h1>".to_owned()),
    }
}

/// The requests the fake web was sent.
type Sent = Arc<Mutex<Vec<HttpRequest>>>;

/// A web of a few pages, in place of the network, and the requests it was sent.
fn web(harness: &Harness) -> Result<Sent, Box<dyn Error>> {
    let sent = Sent::default();
    let seen = Arc::clone(&sent);
    let client: Arc<dyn HttpClient> = Arc::new(move |request: &HttpRequest, _: Duration| {
        seen.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request.clone());
        let (status, kind, body) = page(&request.url);
        let header = if (300..400).contains(&status) {
            "location"
        } else {
            "Content-Type"
        };
        Ok(HttpResponse {
            status,
            headers: vec![(header.to_owned(), kind.to_owned())],
            body: body.into_bytes(),
        })
    });
    harness.http().insert("https", client)?;
    Ok(sent)
}

/// The requests the fake web was sent, in order.
fn asked(sent: &Sent) -> Vec<HttpRequest> {
    sent.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// A `web_fetch` call of `url`.
fn fetch(id: &str, url: &str) -> Message {
    call(id, "web_fetch", &json!({"url": url}))
}

/// What `web_fetch` said to each of `calls`, in a harness whose web is the fake one and
/// whose grant is the host `docs.example`, with the requests that were sent.
async fn fetched(
    name: &str,
    calls: impl IntoIterator<Item = Message>,
) -> Result<(Harness, Vec<String>, Sent), Box<dyn Error>> {
    let work = scratch(name);
    let replies = calls.into_iter().chain([say("read")]);
    let Agent {
        harness,
        model: _model,
    } = agent(replies)?;
    let sent = web(&harness)?;
    harness.plugin(
        load(
            &work,
            "fetch",
            &[("http", Scope::none().allow("docs.example"))],
        )?
        .config(json!({"max_bytes": 200}).to_string()),
    );
    harness.run("read the guide").await?;
    let said = outputs(&harness);
    Ok((harness, said, sent))
}

const GUIDE: &str =
    "Guide\n\n# Install\n\nRun make, then see [the API](https://docs.example/api.html).";

#[tokio::test]
async fn web_fetch_reads_a_page_as_text() {
    let raw = call(
        "6",
        "web_fetch",
        &json!({"url": "https://docs.example/guide", "raw": true}),
    );
    let calls = [
        fetch("1", "https://docs.example/guide"),
        fetch("2", "https://docs.example/data.json"),
        fetch("3", "https://docs.example/logo.png"),
        fetch("4", "https://docs.example/nowhere"),
        fetch("5", "docs.example/guide"),
        raw,
    ];

    let (harness, said, sent) = fetched("addons-fetch", calls).await.unwrap();

    assert_eq!(said[0], GUIDE);
    assert_eq!(said[1], "{\"ok\":true}");
    assert_eq!(
        said[2],
        "https://docs.example/logo.png is image/png, 5 bytes: not text, so it is not shown."
    );
    assert!(
        said[3].contains("https://docs.example/nowhere answered 404:\n# Not here"),
        "{}",
        said[3]
    );
    assert!(
        said[4].contains("whole http or https address"),
        "{}",
        said[4]
    );
    assert!(
        said[5].starts_with("<html><head><title>Guide</title>"),
        "{}",
        said[5]
    );
    let first = &asked(&sent)[0];
    assert!(
        first
            .headers
            .iter()
            .any(|(name, value)| name == "User-Agent" && value.starts_with("corduroy-fetch/"))
    );
    // It reaches out, so a gate that asks about tools that change something asks about it.
    let specs = harness.tools().specs();
    let spec = specs.iter().find(|spec| spec.name == "web_fetch").unwrap();
    assert!(!spec.read_only && spec.replay_safe);
}

#[tokio::test]
async fn web_fetch_follows_redirects_only_within_its_grant() {
    let calls = [
        fetch("1", "https://docs.example/old#install"),
        fetch("2", "https://docs.example/away"),
        fetch("3", "https://docs.example/loop"),
    ];

    let (_harness, said, sent) = fetched("addons-fetch-redirects", calls).await.unwrap();

    assert_eq!(
        said[0],
        format!("[redirected to https://docs.example/guide]\n{GUIDE}")
    );
    // A redirect out of the grant is refused as a request there would be.
    assert!(
        said[1].contains("https://elsewhere.example/x") && said[1].contains("http"),
        "{}",
        said[1]
    );
    assert!(said[2].contains("gave up after 5 redirects"), "{}", said[2]);
    // Nothing went to the host outside the grant, and a fragment is not sent.
    let urls: Vec<String> = asked(&sent)
        .into_iter()
        .map(|request| request.url)
        .collect();
    assert!(
        urls.iter()
            .all(|url| url.starts_with("https://docs.example/"))
    );
    assert_eq!(
        urls[..2],
        ["https://docs.example/old", "https://docs.example/guide"]
    );
}

#[tokio::test]
async fn a_long_page_is_read_a_window_at_a_time() {
    let on = call(
        "2",
        "web_fetch",
        &json!({"url": "https://docs.example/long", "offset": 200}),
    );
    let past = call(
        "3",
        "web_fetch",
        &json!({"url": "https://docs.example/long", "offset": 9000}),
    );
    let calls = [fetch("1", "https://docs.example/long"), on, past];

    let (_harness, said, _sent) = fetched("addons-fetch-long", calls).await.unwrap();

    assert_eq!(
        said[0],
        format!(
            "{}\n[100 more bytes: call again with offset 200]",
            "0123456789".repeat(20)
        )
    );
    assert_eq!(said[1], "0123456789".repeat(10));
    assert_eq!(said[2], "[nothing more: the page ends before that offset]");
}

#[tokio::test]
async fn today_puts_the_date_in_the_prompt_and_offers_the_time() {
    let work = scratch("addons-today");
    let Agent { harness, model } =
        agent([call("1", "now", &json!({})), say("it is late")]).unwrap();
    harness.plugin(
        load(&work, "today", &[("clock", Scope::none().allow("wall"))])
            .unwrap()
            .config(json!({"utc_offset": "+02:00"}).to_string()),
    );

    harness.run("what time is it?").await.unwrap();

    // `Today is Thursday 2026-11-26 (UTC+02:00). …`
    let system = &model.requests()[0].system;
    let today = system
        .split("Today is ")
        .nth(1)
        .unwrap_or_else(|| panic!("{system}"));
    let words: Vec<&str> = today.split(' ').collect();
    assert!(words[0].ends_with("day"), "{today}");
    assert_eq!(words[1].len(), 10, "{today}");
    assert!(words[1].starts_with("20"), "{today}");
    assert_eq!(words[2], "(UTC+02:00).", "{today}");
    // `Thursday 2026-11-26 14:03:22 (UTC+02:00)`
    let now = &outputs(&harness)[0];
    let words: Vec<&str> = now.split(' ').collect();
    assert_eq!(words[1], today.split(' ').nth(1).unwrap(), "{now}");
    assert_eq!(words[2].len(), 8, "{now}");
    assert_eq!(words[3], "(UTC+02:00)", "{now}");
}

#[tokio::test]
async fn today_does_not_start_with_an_offset_it_cannot_read() {
    let work = scratch("addons-today-offset");
    let Agent {
        harness,
        model: _model,
    } = agent([say("ok")]).unwrap();
    harness.plugin(
        load(&work, "today", &[("clock", Scope::any())])
            .unwrap()
            .config(json!({"utc_offset": "Europe/Berlin"}).to_string()),
    );

    let failed = harness.events().matching(&Pattern::new("plugin/failed"));
    assert!(
        failed[0].payload.contains("utc_offset"),
        "{}",
        failed[0].payload
    );
    assert!(
        failed[0].payload.contains("+01:00"),
        "{}",
        failed[0].payload
    );
}
