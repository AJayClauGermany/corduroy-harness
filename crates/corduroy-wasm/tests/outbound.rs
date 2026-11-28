//! The capabilities that reach past the plugin tree: `llm`, `secret`, `http` and `log-read`.
//! Models and HTTP clients are fakes registered natively; no network or LLM. Uses the
//! `testbed` plugin from `plugins/`; build it first with `cargo plugins`.

use std::error::Error;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use corduroy_core::{
    App, Draft, EventStream, GATES, Gates, HTTP, HttpClient, HttpClients, HttpRequest,
    HttpResponse, MODELS, Message, Model, ModelError, ModelReply, ModelRequest, Models, PROMPTS,
    Pattern, Prompts, SECRETS, SERVICES, Scope, Secret, Secrets, Services, SessionId, Source,
    StopReason, TOOLS, Tools, Usage,
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
        &host.tools.call(tool, &input.to_string())?.to_text(),
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
    // Registered once, under the provider's name, for every model of it.
    let local: Arc<dyn Model> = Arc::new(|request: &ModelRequest, timeout: Duration| {
        if request.model == "broken" {
            return Err(ModelError::Failed("overloaded".into()));
        }
        let prompt = request.messages[0].text_content();
        let said = format!(
            "{} heard {prompt}, within {}ms",
            request.model,
            timeout.as_millis()
        );
        let usage = Usage::new(Some(12), Some(7)).with_reasoning(Some(3));
        Ok(ModelReply::new(Message::assistant().text(said), usage).stopped(StopReason::Finished))
    });
    host.models.insert("local", local).unwrap();
    let grant = Scope::none().allow("local/*").deny("local/large");
    host.app.plugin(testbed(vec![("llm", grant)]).unwrap());

    let answered = call(
        &host,
        "ask",
        &json!({"model": "local/small", "system": "terse", "prompt": "hello"}),
    )
    .unwrap();
    let refused = call(
        &host,
        "ask",
        &json!({"model": "local/large", "prompt": "hi"}),
    )
    .unwrap();
    let elsewhere = call(
        &host,
        "ask",
        &json!({"model": "other/small", "prompt": "hi"}),
    )
    .unwrap();
    let failed = call(
        &host,
        "ask",
        &json!({"model": "local/broken", "prompt": "hi"}),
    )
    .unwrap();

    // The model is told its provider's own name for it, and the host's bound on the wait.
    assert_eq!(answered, "small heard hello, within 1500ms");
    // The grant is the one list of models: "all of this provider's but one" is a grant.
    assert!(
        refused["denied"].as_str().unwrap().contains("local/large"),
        "{refused}"
    );
    assert!(
        elsewhere["denied"]
            .as_str()
            .unwrap()
            .contains("other/small"),
        "{elsewhere}"
    );
    assert_eq!(failed["failed"], "the model failed: overloaded");
    let requests = payloads(&host.app, "llm/request");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["model"], "local/small");
    assert_eq!(requests[0]["system"], "terse");
    assert_eq!(
        requests[0]["messages"][0],
        json!({"role": "user", "text": "hello"})
    );
    // What a plugin's own model call cost is in the record, with why the model stopped.
    let responses = payloads(&host.app, "llm/response");
    assert_eq!(
        responses[0],
        json!({
            "model": "local/small", "ok": true, "output": "small heard hello, within 1500ms",
            "input_tokens": 12, "output_tokens": 7, "reasoning_tokens": 3, "stop": "finished"
        })
    );
    assert_eq!(
        responses[1],
        json!({"model": "local/broken", "ok": false, "output": "the model failed: overloaded"})
    );
}

#[test]
fn a_model_named_without_its_provider_is_unknown() {
    let mut host = host(EventStream::new()).unwrap();
    host.app
        .plugin(testbed(vec![("llm", Scope::any())]).unwrap());

    let asked = call(&host, "ask", &json!({"model": "small", "prompt": "hi"})).unwrap();

    let why = asked["failed"].as_str().unwrap();
    assert!(why.contains("unknown model `small`"), "{why}");
    assert!(why.contains("<provider>/<model>"), "{why}");
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
fn a_secret_that_names_its_hosts_is_written_into_requests_to_those_only() {
    let mut host = host(EventStream::new()).unwrap();
    let token: Arc<dyn Secret> = Arc::new(|| Ok("s3cret-value".to_owned()));
    let bound = corduroy_core::secret::to_hosts(token, Scope::none().allow("api.example.com"));
    host.secrets.insert("token", bound).unwrap();
    // The add-on's own grant is wide: any host, with the secret.
    host.app.plugin(
        testbed(vec![
            ("http", Scope::any()),
            ("secret", Scope::none().allow("token")),
        ])
        .unwrap(),
    );
    let fetch = |url: &str, headers: Value| {
        call(
            &host,
            "fetch",
            &json!({"method": "GET", "url": url, "headers": headers}),
        )
        .unwrap()
    };
    let with_token = || json!([{"name": "authorization", "secret": "token"}]);

    let named = fetch("https://api.example.com/", with_token());
    let elsewhere = fetch("https://evil.example.org/", with_token());
    let without = fetch("https://evil.example.org/", json!([]));

    assert_eq!(named["status"], 200);
    assert_eq!(
        elsewhere["denied"],
        "secret `token` does not name `evil.example.org` among the hosts it may be sent to"
    );
    // The host itself is within the grant: what was refused is the secret going there.
    assert_eq!(without["status"], 200);
    let sent = host.sent.lock().unwrap();
    assert_eq!(sent.len(), 2);
    assert!(
        sent.iter()
            .filter(|request| request.url.contains("evil"))
            .all(|request| request.headers.is_empty())
    );
    drop(sent);
    assert_eq!(
        payloads(&host.app, "secret/refused"),
        [json!({"plugin": "testbed", "secret": "token", "host": "evil.example.org"})]
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
    // A parser would read the backslash as `/` and the host as `evil.org`.
    for tricky in [
        "https://evil.org\\.api.example.com/",
        "https://api.example.com\t/",
    ] {
        let refused = call(&host, "fetch", &json!({"method": "GET", "url": tricky})).unwrap();
        assert!(
            refused["failed"].as_str().unwrap().contains("backslash"),
            "{refused}"
        );
    }
    assert_eq!(host.sent.lock().unwrap().len(), 0);
    assert_eq!(payloads(&host.app, "http/request"), Vec::<Value>::new());
}

#[test]
fn a_plugin_reads_only_the_sessions_and_topics_its_grant_reaches() {
    let events = EventStream::new();
    for (session, topic) in [
        ("a", "message"),
        ("b", "message"),
        ("a", "tool/call"),
        ("a:b", "message"),
    ] {
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
    let nested = call(&host, "read", &json!({"topic": "*", "session": "a:b"})).unwrap();
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
    // `a:*` reaches session `a`, not a session whose id merely starts with `a:`.
    assert_eq!(nested, json!([]));
    assert_eq!(paged, json!([[3, "a", "tool/call"]]));
}

#[test]
fn a_plugin_cannot_write_its_state_store_through_events() {
    let mut host = host(EventStream::new()).unwrap();
    let config = json!({"echo": {"on": "ping", "emit": "state/set"}});
    host.app.plugin(
        testbed(vec![("events", Scope::any())])
            .unwrap()
            .config(config.to_string()),
    );

    host.app
        .events()
        .emit(Draft::new(Source::Host, "ping").payload(r#"{"key":"k","value":"forged"}"#))
        .unwrap();

    assert_eq!(payloads(&host.app, "state/set"), Vec::<Value>::new());
}
