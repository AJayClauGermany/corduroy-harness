//! The page served: who is let in, what the page loads, and an answer given on it reaching
//! the run. A scripted model, a gate that asks, and a browser that is a socket; no network
//! beyond the loopback interface, no LLM.

use std::error::Error;
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use corduroy_harness::core::{
    EventStream, GATES, Gate, Gates, Message, Pattern, Plugin, SessionId, TOOL_CALL, TOOLS,
    ToolSpec, Tools, Verdict, plugin_fn, tool_fn,
};
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use corduroy_harness::{Harness, MAIN, Status};
use corduroy_page::{Config, page};
use serde_json::{Value, json};

/// `poke`, which changes things and counts the times it ran, behind a gate that asks.
fn asked_about(runs: &Arc<AtomicUsize>) -> impl Plugin + 'static {
    let runs = Arc::clone(runs);
    plugin_fn("asked-about", &[TOOLS, GATES], move |ctx| {
        let runs = Arc::clone(&runs);
        let spec = ToolSpec::new("poke", "", r#"{"type":"object"}"#);
        let tool = tool_fn(spec, move |_| {
            runs.fetch_add(1, Ordering::SeqCst);
            Ok("poked".to_owned())
        });
        ctx.service::<Tools>(TOOLS)?
            .register(ctx, "poke", Arc::new(tool))?;
        let gate: Arc<dyn Gate> = Arc::new(|point: &str, _: &str| {
            (point == TOOL_CALL).then(|| Verdict::Ask("poke changes things".into()))
        });
        ctx.service::<Gates>(GATES)?
            .register(ctx, "careful", gate)?;
        Ok(())
    })
}

/// A page being served, and the harness it is the page of.
struct Open {
    harness: Harness,
    /// `127.0.0.1:<port>`.
    host: String,
    /// The token its address holds.
    token: String,
    /// How many times `poke` ran.
    runs: Arc<AtomicUsize>,
}

/// A harness whose model gives `replies`, with the page loaded and opened.
fn open(replies: impl IntoIterator<Item = Message>) -> Result<Open, Box<dyn Error>> {
    let runs = Arc::new(AtomicUsize::new(0));
    let model = Arc::new(ScriptedProvider::new(replies));
    let harness = Harness::resume(model, Arc::new(EventStream::new()))?;
    harness.plugin(asked_about(&runs));
    harness.plugin(page("page", Config::new()));
    let front = harness
        .fronts()
        .get("page")
        .ok_or("no front was registered")?;
    let line = front.open()?;
    // `… http://127.0.0.1:<port>/<token>/ …`
    let address = line
        .split_whitespace()
        .find_map(|word| word.strip_prefix("http://"))
        .ok_or("the line holds no address")?;
    let mut parts = address.split('/');
    let (host, token) = (parts.next(), parts.next());
    Ok(Open {
        harness,
        host: host.ok_or("no host")?.to_owned(),
        token: token.ok_or("no token")?.to_owned(),
        runs,
    })
}

/// What came back: the status, the head as it was sent, and the body.
struct Got {
    status: u16,
    head: String,
    body: String,
}

impl Open {
    /// Send `request`, as written, and read the answer.
    fn send(&self, request: &str) -> Result<Got, Box<dyn Error>> {
        let mut stream = TcpStream::connect(&self.host)?;
        stream.write_all(request.as_bytes())?;
        let mut answer = String::new();
        stream.read_to_string(&mut answer)?;
        let (head, body) = answer.split_once("\r\n\r\n").ok_or("no head")?;
        let status = head.split(' ').nth(1).ok_or("no status")?.parse()?;
        Ok(Got {
            status,
            head: head.to_owned(),
            body: body.to_owned(),
        })
    }

    /// Fetch `path` as a browser at the page's address would.
    fn get(&self, path: &str) -> Result<Got, Box<dyn Error>> {
        self.send(&format!(
            "GET {path} HTTP/1.1\r\nHost: {}\r\n\r\n",
            self.host
        ))
    }

    /// Fetch `file` of the page.
    fn file(&self, file: &str) -> Result<Got, Box<dyn Error>> {
        self.get(&format!("/{}/{file}", self.token))
    }

    /// Post `body` as an answer, saying it comes from the page at `origin`.
    fn answer(&self, origin: Option<&str>, body: &Value) -> Result<Got, Box<dyn Error>> {
        let body = body.to_string();
        let origin = origin.map_or_else(String::new, |origin| format!("Origin: {origin}\r\n"));
        self.send(&format!(
            "POST /{}/answer HTTP/1.1\r\nHost: {}\r\n{origin}Content-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n{body}",
            self.token,
            self.host,
            body.len()
        ))
    }

    /// The page's own origin.
    fn origin(&self) -> String {
        format!("http://{}", self.host)
    }

    fn recorded(&self, topic: &str) -> Vec<Value> {
        self.harness
            .events()
            .matching(&Pattern::new(topic))
            .iter()
            .filter_map(|event| serde_json::from_str(&event.payload).ok())
            .collect()
    }
}

/// The JSON a script of the page hands to `function`.
fn handed(script: &str, function: &str) -> Result<Value, Box<dyn Error>> {
    let json = script
        .trim()
        .strip_prefix(function)
        .and_then(|rest| rest.strip_prefix('('))
        .and_then(|rest| rest.strip_suffix(");"))
        .ok_or("not that script")?;
    Ok(serde_json::from_str(&format!("[{json}]"))?)
}

#[test]
fn the_page_is_served_to_whoever_has_its_address_and_to_nobody_else() {
    let open = open([]).unwrap();
    assert_eq!(open.token.len(), 32);

    let page = open.file("").unwrap();
    assert_eq!(page.status, 200);
    assert!(
        page.body.contains("<title>corduroy</title>"),
        "{}",
        page.body
    );
    // It may send an answer back, and reach nothing else.
    assert!(page.head.contains("connect-src 'self'"), "{}", page.head);
    assert!(page.body.contains("connect-src 'self'") && !page.body.contains("file:"));
    assert!(
        page.head.contains("Cache-Control: no-store"),
        "{}",
        page.head
    );
    for (file, kind) in [("app.js", "text/javascript"), ("style.css", "text/css")] {
        let got = open.file(file).unwrap();
        assert_eq!(got.status, 200, "{file}");
        assert!(got.head.contains(kind), "{}", got.head);
    }

    // Without the token, or with one that is nearly it, nothing says which was wrong.
    let nearly = format!("/{}0/", open.token.chars().take(31).collect::<String>());
    for path in ["/", "/index.html", "/app.js", nearly.as_str(), "/../"] {
        let got = open.get(path).unwrap();
        assert_eq!(got.status, 403, "{path}");
        assert!(!got.body.contains(&open.token));
    }
    // A name that merely points here is not this machine's own address.
    let elsewhere = format!(
        "GET /{}/ HTTP/1.1\r\nHost: evil.example\r\n\r\n",
        open.token
    );
    assert_eq!(open.send(&elsewhere).unwrap().status, 403);
    let unnamed = format!("GET /{}/ HTTP/1.1\r\n\r\n", open.token);
    assert_eq!(open.send(&unnamed).unwrap().status, 403);

    let door = open.get(&format!("/{}", open.token)).unwrap();
    assert_eq!(door.status, 301);
    assert!(door.head.contains(&format!("Location: /{}/", open.token)));
    assert_eq!(open.file("secrets.txt").unwrap().status, 404);
    assert_eq!(open.file("answer").unwrap().status, 405);
    assert_eq!(open.send("nonsense\r\n\r\n").unwrap().status, 400);
}

#[test]
fn opening_twice_says_the_same_and_unloading_stops_the_page() {
    let open = open([]).unwrap();
    let front = open.harness.fronts().get("page").unwrap();
    let again = front.open().unwrap();
    assert!(again.contains(&format!("http://{}/{}/", open.host, open.token)));
    assert_eq!(open.file("").unwrap().status, 200);

    let loaded = open
        .harness
        .app()
        .fibers()
        .into_iter()
        .find(|fiber| fiber.name == "page")
        .unwrap();
    open.harness.app().dispose(loaded.id).unwrap();

    assert!(open.harness.fronts().get("page").is_none());
    // Nothing listens there any more, or what does is not the page.
    assert!(open.file("").map_or(true, |got| got.status != 200));
}

#[tokio::test]
async fn the_events_are_served_as_the_scripts_the_page_loads() {
    let open = open([say("hello")]).unwrap();
    open.harness.run("say hello").await.unwrap();

    let index = &handed(&open.file("index.js?t=1").unwrap().body, "corduroyIndex").unwrap()[0];
    let stream = open.harness.events().events();
    assert_eq!(index["format"], json!(1));
    assert_eq!(index["chunks"], json!(1));
    assert_eq!(index["events"], json!(stream.len()));
    assert_eq!(index["last"], json!(stream.last().unwrap().id.get()));

    let chunk = handed(
        &open.file("events-000000.js").unwrap().body,
        "corduroyChunk",
    )
    .unwrap();
    assert_eq!(chunk[0], json!(0));
    let events = chunk[1].as_array().unwrap();
    let ids: Vec<u64> = events
        .iter()
        .filter_map(|event| event["id"].as_u64())
        .collect();
    assert_eq!(
        ids,
        stream
            .iter()
            .map(|event| event.id.get())
            .collect::<Vec<_>>()
    );
    let prompt = events
        .iter()
        .find(|event| event["topic"] == "message" && event["source"] == "user")
        .unwrap();
    assert_eq!(prompt["payload"]["parts"][0]["text"], json!("say hello"));
    assert_eq!(prompt["session"], json!(MAIN));
    // A chunk past the end is empty, not an error.
    let past = handed(
        &open.file("events-000007.js").unwrap().body,
        "corduroyChunk",
    )
    .unwrap();
    assert_eq!(past[1], json!([]));
}

#[tokio::test]
async fn an_answer_given_on_the_page_is_recorded_and_the_host_takes_the_run_on() {
    let open = open([call("c1", "poke", &json!({})), say("poked")]).unwrap();
    let main = SessionId::new(MAIN);
    let waiting = open.harness.run("poke it").await.unwrap();
    assert!(matches!(waiting.status, Status::Waiting { .. }));
    let allow = json!({"session": MAIN, "call": "c1", "allow": true});

    // Only the page itself may answer: the browser says where a request comes from.
    for origin in [
        None,
        Some("http://evil.example"),
        Some("http://127.0.0.1:1"),
    ] {
        assert_eq!(
            open.answer(origin, &allow).unwrap().status,
            403,
            "{origin:?}"
        );
    }
    let origin = open.origin();
    let vague = open.answer(Some(&origin), &json!({"call": "c1"})).unwrap();
    assert_eq!(vague.status, 400);
    let nowhere = json!({"session": "nowhere", "call": "c1", "allow": true});
    assert_eq!(open.answer(Some(&origin), &nowhere).unwrap().status, 404);
    assert_eq!(open.recorded("gate/answer"), Vec::<Value>::new());

    let answered = open.answer(Some(&origin), &allow).unwrap();

    assert_eq!(
        (answered.status, answered.body.as_str()),
        (200, r#"{"ok":true}"#)
    );
    // Recorded as the person's, through the page, and nothing run by the page.
    let recorded = &open.recorded("gate/answer")[0];
    assert_eq!(recorded["allow"], json!(true));
    assert_eq!(recorded["via"], json!("page"));
    assert_eq!(open.runs.load(Ordering::SeqCst), 0);
    // Answered once: the same again finds nothing waiting.
    let again = open.answer(Some(&origin), &allow).unwrap();
    assert_eq!(again.status, 409);
    assert!(again.body.contains("answered already"), "{}", again.body);

    let outcome = open.harness.answered(&main).await.unwrap();
    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "poked".into()
        }
    );
    assert_eq!(open.runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_refusal_on_the_page_carries_its_reason_to_the_model() {
    let open = open([
        call("c1", "poke", &json!({})),
        say("left alone"),
        call("c2", "poke", &json!({})),
        say("left alone again"),
    ])
    .unwrap();
    let (main, origin) = (SessionId::new(MAIN), open.origin());

    open.harness.run("poke it").await.unwrap();
    let refuse = json!({"session": MAIN, "call": "c1", "allow": false, "reason": " not now "});
    assert_eq!(open.answer(Some(&origin), &refuse).unwrap().status, 200);
    open.harness.answered(&main).await.unwrap();
    open.harness.run("poke it again").await.unwrap();
    let silent = json!({"session": MAIN, "call": "c2", "allow": false});
    assert_eq!(open.answer(Some(&origin), &silent).unwrap().status, 200);
    open.harness.answered(&main).await.unwrap();

    let recorded = open.recorded("gate/answer");
    assert_eq!(recorded[0]["reason"], json!("not now"));
    assert_eq!(recorded[1]["reason"], json!("the user declined"));
    assert_eq!(open.runs.load(Ordering::SeqCst), 0);
    let results = open.recorded("tool/result");
    assert!(results[0]["output"].as_str().unwrap().contains("not now"));
}
