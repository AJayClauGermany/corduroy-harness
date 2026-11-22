//! The capabilities that reach past the plugin tree: `llm`, `secret`, `http` and `log-read`.
//! Models and HTTP clients are fakes registered natively; no network or LLM. Uses the
//! `testbed` plugin from `plugins/`; build it first with `cargo plugins`.

use std::error::Error;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use corduroy_core::{
    App, Completion, Draft, EventStream, GATES, Gates, HTTP, HttpClient, HttpClients, HttpRequest,
    HttpResponse, MODELS, Model, ModelError, Models, PROMPTS, Pattern, Prompts, SECRETS, SERVICES,
    Scope, Secret, Secrets, Services, SessionId, Source, TOOLS, Tools,
};
use corduroy_test_support::plugin;
use corduroy_wasm::{Limits, WasmPlugin, WasmRuntime};
use serde_json::{Value, json};

struct Host {
    app: App,
    tools: Arc<Tools>,
    models: Arc<Models>,
    secrets: Arc<Secrets>,
    /// What the `https` client was sent.
    sent: Sent,
}

fn host(events: EventStream) -> Result<Host, Box<dyn Error>> {
    let tools = Arc::new(Tools::new());
    let models = Arc::new(Models::new());
    let secrets = Arc::new(Secrets::new());
    let http = Arc::new(HttpClients::new());
    let mut app = App::with_events(Arc::new(events));
    app.provide(TOOLS, Arc::clone(&tools))?;
    app.provide(PROMPTS, Arc::new(Prompts::new()))?;
    app.provide(GATES, Arc::new(Gates::new()))?;
    app.provide(SERVICES, Arc::new(Services::new()))?;
    app.provide(MODELS, Arc::clone(&models))?;
    app.provide(SECRETS, Arc::clone(&secrets))?;
    app.provide(HTTP, Arc::clone(&http))?;
    let sent = recording(&http)?;
    Ok(Host {
        app,
        tools,
        models,
        secrets,
        sent,
    })
}

/// The testbed with `grants`; everything else it imports is linked with an empty scope.
fn testbed(grants: Vec<(&str, Scope)>) -> Result<WasmPlugin, Box<dyn Error>> {
    let limits = Limits {
        outbound_wait: Duration::from_millis(1500),
        ..Limits::default()
    };
    let mut testbed = WasmRuntime::new()?
        .with_limits(limits)
        .load(plugin("testbed"))?;
    for capability in testbed.requires().to_vec() {
        testbed = testbed.grant(capability, Scope::none());
    }
    for (capability, scope) in grants {
        testbed = testbed.grant(capability, scope);
    }
    Ok(testbed)
}

fn call(host: &Host, tool: &str, input: &Value) -> Result<Value, Box<dyn Error>> {
    Ok(serde_json::from_str(
        &host.tools.call(tool, &input.to_string())?,
    )?)
}

fn payloads(app: &App, topic: &str) -> Vec<Value> {
    app.events()
        .matching(&Pattern::new(topic))
        .iter()
        .filter_map(|event| serde_json::from_str(&event.payload).ok())
        .collect()
}

#[test]
fn a_plugin_asks_the_models_its_grant_names_and_the_exchange_is_recorded() {
    let mut host = host(EventStream::new()).unwrap();
    let small: Arc<dyn Model> = Arc::new(|request: &Completion, timeout: Duration| {
        let prompt = &request.messages[0].text;
        Ok(format!("{prompt}, within {}ms", timeout.as_millis()))
    });
    host.models.insert("small", small).unwrap();
    let broken: Arc<dyn Model> =
        Arc::new(|_: &Completion, _: Duration| Err(ModelError::Failed("overloaded".into())));
    host.models.insert("broken", broken).unwrap();
    host.app
        .plugin(testbed(vec![("llm", Scope::none().allow("small").allow("broken"))]).unwrap());

    let answered = call(
        &host,
        "ask",
        &json!({"model": "small", "system": "terse", "prompt": "hello"}),
    )
    .unwrap();
    let refused = call(&host, "ask", &json!({"model": "large", "prompt": "hi"})).unwrap();
    let failed = call(&host, "ask", &json!({"model": "broken", "prompt": "hi"})).unwrap();

    // The host passes its own bound on the wait to the model.
    assert_eq!(answered, "hello, within 1500ms");
    assert!(
        refused["denied"].as_str().unwrap().contains("large"),
        "{refused}"
    );
    assert_eq!(failed["failed"], "the model failed: overloaded");
    let requests = payloads(&host.app, "llm/request");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["system"], "terse");
    assert_eq!(
        requests[0]["messages"][0],
        json!({"role": "user", "text": "hello"})
    );
    let responses = payloads(&host.app, "llm/response");
    assert_eq!(responses[0]["ok"], true);
    assert_eq!(responses[1]["ok"], false);
}

/// The requests a client was sent.
type Sent = Arc<Mutex<Vec<HttpRequest>>>;

/// A client for `https` that answers every request with 200 and remembers what it was sent.
fn recording(http: &HttpClients) -> Result<Sent, Box<dyn Error>> {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&sent);
    let client: Arc<dyn HttpClient> = Arc::new(move |request: &HttpRequest, _: Duration| {
        seen.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request.clone());
        Ok(HttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/plain".into())],
            body: b"pong".to_vec(),
        })
    });
    http.insert("https", client)?;
    Ok(sent)
}

/// A host with the secrets `token` and `other`, and the testbed allowed
/// to reach `api.example.com` with `token`.
fn fetcher() -> Result<Host, Box<dyn Error>> {
    let mut host = host(EventStream::new())?;
    let token: Arc<dyn Secret> = Arc::new(|| Ok("s3cret-value".to_owned()));
    host.secrets.insert("token", token)?;
    let other: Arc<dyn Secret> = Arc::new(|| Ok("other-value".to_owned()));
    host.secrets.insert("other", other)?;
    host.app.plugin(testbed(vec![
        ("http", Scope::none().allow("api.example.com")),
        ("secret", Scope::none().allow("token")),
    ])?);
    Ok(host)
}

#[test]
fn a_plugin_sends_requests_to_its_hosts_with_secrets_it_never_sees() {
    let host = fetcher().unwrap();
    let headers = json!([
        {"name": "accept", "text": "text/plain"},
        {"name": "authorization", "secret": "token"},
    ]);

    let fetched = call(
        &host,
        "fetch",
        &json!({"method": "POST", "url": "https://API.example.com:443/ping?x=1", "headers": headers, "body": "ping"}),
    )
    .unwrap();

    assert_eq!(fetched, json!({"status": 200, "body": "pong"}));
    let request = host.sent.lock().unwrap()[0].clone();
    assert_eq!(request.method, "POST");
    assert_eq!(
        request.headers,
        [
            ("accept".to_owned(), "text/plain".to_owned()),
            ("authorization".to_owned(), "s3cret-value".to_owned())
        ]
    );
    assert_eq!(request.body.as_deref(), Some(&b"ping"[..]));
    let recorded = &payloads(&host.app, "http/request")[0];
    assert_eq!(
        recorded["headers"][1],
        json!({"name": "authorization", "secret": "token"})
    );
    assert_eq!(payloads(&host.app, "http/response")[0]["status"], 200);
    // The value is nowhere on the stream.
    assert!(
        host.app
            .events()
            .events()
            .iter()
            .all(|event| !event.payload.contains("s3cret-value"))
    );
}

#[test]
fn requests_beyond_the_grant_are_refused_before_they_are_sent() {
    let host = fetcher().unwrap();

    let elsewhere = call(
        &host,
        "fetch",
        &json!({"method": "GET", "url": "https://evil.example.org/"}),
    )
    .unwrap();
    let ungranted = call(
        &host,
        "fetch",
        &json!({"method": "GET", "url": "https://api.example.com/", "headers": [{"name": "x", "secret": "other"}]}),
    )
    .unwrap();
    let disguised = call(
        &host,
        "fetch",
        &json!({"method": "GET", "url": "https://api.example.com@evil.example.org/"}),
    )
    .unwrap();
    let plain = call(
        &host,
        "fetch",
        &json!({"method": "GET", "url": "http://api.example.com/"}),
    )
    .unwrap();

    assert!(
        elsewhere["denied"]
            .as_str()
            .unwrap()
            .contains("evil.example.org"),
        "{elsewhere}"
    );
    assert!(
        ungranted["denied"].as_str().unwrap().contains("other"),
        "{ungranted}"
    );
    assert!(
        disguised["failed"].as_str().unwrap().contains("user name"),
        "{disguised}"
    );
    assert_eq!(plain["failed"], "the host has no HTTP client for `http`");
    assert_eq!(host.sent.lock().unwrap().len(), 0);
    assert_eq!(payloads(&host.app, "http/request"), Vec::<Value>::new());
}

#[test]
fn a_plugin_reads_only_the_sessions_and_topics_its_grant_reaches() {
    let events = EventStream::new();
    for (session, topic) in [("a", "message"), ("b", "message"), ("a", "tool/call")] {
        let draft = Draft::new(Source::User, topic).session(SessionId::new(session));
        events.emit(draft).unwrap();
    }
    events
        .emit(Draft::new(Source::Host, "plugin/activated"))
        .unwrap();
    let mut host = host(events).unwrap();
    host.app
        .plugin(testbed(vec![("log-read", Scope::none().allow("a:*"))]).unwrap());

    let everything = call(&host, "read", &json!({"topic": "*"})).unwrap();
    let messages = call(&host, "read", &json!({"topic": "message", "session": "a"})).unwrap();
    let other = call(&host, "read", &json!({"topic": "*", "session": "b"})).unwrap();
    let paged = call(
        &host,
        "read",
        &json!({"topic": "*", "after": 1, "limit": 1}),
    )
    .unwrap();

    assert_eq!(
        everything,
        json!([[1, "a", "message"], [3, "a", "tool/call"]])
    );
    assert_eq!(messages, json!([[1, "a", "message"]]));
    assert_eq!(other, json!([]));
    assert_eq!(paged, json!([[3, "a", "tool/call"]]));
}
