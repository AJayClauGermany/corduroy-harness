//! A plugin for the host's own tests, not for agents: it exercises host calls from inside a
//! component and misbehaves on request.
//!
//! | Tool | Does |
//! | --- | --- |
//! | `probe` | Logs a line, reads both clocks and draws 16 random bytes; returns what each gave, or the error |
//! | `count` | Adds one to a counter held in the instance's memory and returns it |
//! | `trap` | Traps, which kills the instance |
//! | `relay` | Calls a tool (`{"tool", "input"}`) or a service (`{"service", "method", "payload"}`) and returns what it gave, or the error. A tool's result that is not one block of text is also there block by block, as `blocks`: `["text", text]` and `["image", mime type, [bytes]]` |
//! | `picture` | Returns a caption and a small image (`{"bytes": [..]}` to choose its bytes) as two content blocks |
//! | `state` | `{"op": "get" \| "set" \| "remove" \| "keys", "key", "value"}` on its `state` store |
//! | `load` | Loads a plugin: `{"path", "name", "config", "grants": [{"capability", "allow", "deny"}]}` |
//! | `unload` | Unloads a plugin it loaded: `{"name"}` |
//! | `loaded` | Lists the plugins it loaded |
//! | `ask` | Asks a model: `{"model", "system", "prompt"}` |
//! | `fetch` | Makes an HTTP request: `{"method", "url", "body", "headers": [{"name", "text"} \| {"name", "secret"}]}`; returns `{"status", "body"}` |
//! | `read` | Reads the event stream: `{"session", "topic", "after", "limit"}`; returns `[[id, session, topic]]` |
//! | `tools` | Lists the tools its `tools` scope reaches, by name |
//! | `session` | `{"current", "call", "stopping"}`: what `session` says about the call it serves |
//!
//! `state` with `"trap": true` traps after the operation, to show that a trapped call's writes
//! are not kept.
//!
//! Configuration: `{"provides": [..]}` names the services it answers (`echo` returns the call
//! as JSON; `relay` makes the call its payload describes); `{"prefix": "a."}` puts a prefix
//! before each tool's name, so that two instances can be loaded side by side;
//! `{"gates": [..]}` names decision points to gate, which the host must refuse for
//! `tool/call` and `plugin/load` since this plugin also imports `tools` and `plugins`; and
//! `{"echo": {"on": "ping", "emit": "pong"}}` emits `pong` for every `ping` it sees, so that
//! two instances can be set off against each other; with `"session": true` the event it
//! emits is `{"session"}`, the session it was told it serves while handling the event.

use std::fmt::Write as _;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use corduroy_guest::host::log::Level;
use corduroy_guest::host::{
    clock, events, http, llm, log, log_read, plugins, random, secret, service, session, state,
    tools,
};
use corduroy_guest::{
    CallError, Content, Event, HostError, Plugin, PluginInfo, ServiceCall, ToolError, ToolSpec,
    text_of,
};
use serde_json::{Value, json};

/// Lives in the instance's memory, so it starts again from zero in a new instance.
static COUNT: AtomicU64 = AtomicU64::new(0);

/// Put before each tool's name.
static PREFIX: OnceLock<String> = OnceLock::new();

/// The topic to emit for every event this plugin is sent.
static ECHO: OnceLock<String> = OnceLock::new();

/// Whether the echo says which session it was handled for, instead of repeating the payload.
static ECHO_SESSION: OnceLock<bool> = OnceLock::new();

/// A host call's answer, or its error, as JSON.
fn report<T: Into<Value>>(result: Result<T, HostError>) -> Value {
    match result {
        Ok(value) => value.into(),
        Err(HostError::Denied(reason)) => json!({"denied": reason}),
        Err(HostError::Failed(reason)) => json!({"failed": reason}),
    }
}

fn probe() -> String {
    let logged = report(log::write(Level::Info, "probing the host").map(|()| true));
    let now = report(clock::now().map(|now| json!([now.seconds, now.nanoseconds])));
    let first = clock::monotonic();
    let second = clock::monotonic();
    let elapsed = report(first.and_then(|first| second.map(|second| second >= first)));
    let random = report(random::get_bytes(16).map(|bytes| {
        bytes.iter().fold(String::new(), |mut hex, byte| {
            // Writing to a `String` cannot fail.
            let _ = write!(hex, "{byte:02x}");
            hex
        })
    }));
    json!({"logged": logged, "now": now, "monotonic": elapsed, "random": random}).to_string()
}

/// A string field of `input`, or an empty string.
fn text<'a>(input: &'a Value, field: &str) -> &'a str {
    input.get(field).and_then(Value::as_str).unwrap_or_default()
}

/// What `picture` shows unless told otherwise: the eight bytes every PNG file starts with.
const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// A caption and an image, as two content blocks.
fn picture(input: &Value) -> Vec<Content> {
    let bytes = input.get("bytes").and_then(Value::as_array).map_or_else(
        || PNG.to_vec(),
        |bytes| {
            let bytes = bytes.iter().filter_map(Value::as_u64);
            bytes.filter_map(|byte| u8::try_from(byte).ok()).collect()
        },
    );
    vec![
        Content::text(format!("a picture of {} bytes", bytes.len())),
        Content::image("image/png", bytes),
    ]
}

/// What a tool returned, for `relay` to pass on: its text, and each block as it arrived
/// unless the result is one block of text.
fn relayed(blocks: &[Content]) -> Value {
    let mut said = json!({"ok": text_of(blocks)});
    if !matches!(blocks, [Content::Text(_)]) {
        let blocks: Vec<Value> = blocks
            .iter()
            .map(|block| match block {
                Content::Text(text) => json!(["text", text]),
                Content::Image(image) => json!(["image", image.mime_type, image.data]),
            })
            .collect();
        if let Some(said) = said.as_object_mut() {
            said.insert("blocks".to_owned(), Value::Array(blocks));
        }
    }
    said
}

/// Make the tool or service call `input` describes, and say what came of it.
fn relay(input: &Value) -> String {
    let outcome = if let Some(tool) = input.get("tool").and_then(Value::as_str) {
        let arguments = input.get("input").cloned().unwrap_or_else(|| json!({}));
        tools::call(tool, &arguments.to_string())
            .map(|blocks| relayed(&blocks))
            .map_err(|error| format!("{error:?}"))
    } else {
        service::call(
            text(input, "service"),
            text(input, "method"),
            text(input, "payload"),
        )
        .map(|output| json!({"ok": output}))
        .map_err(|error| format!("{error:?}"))
    };
    outcome
        .unwrap_or_else(|error| json!({"error": error}))
        .to_string()
}

/// A list of strings in `input`, or an empty one.
fn strings(input: &Value, field: &str) -> Vec<String> {
    input
        .get(field)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn load(input: &Value) -> String {
    let grants = input
        .get("grants")
        .and_then(Value::as_array)
        .map(|grants| {
            grants
                .iter()
                .map(|grant| plugins::Grant {
                    capability: text(grant, "capability").to_owned(),
                    allow: strings(grant, "allow"),
                    deny: strings(grant, "deny"),
                })
                .collect()
        })
        .unwrap_or_default();
    let request = plugins::LoadRequest {
        name: text(input, "name").to_owned(),
        path: text(input, "path").to_owned(),
        config: input
            .get("config")
            .map_or_else(|| "{}".to_owned(), Value::to_string),
        grants,
    };
    report(plugins::load(&request)).to_string()
}

fn store(input: &Value) -> String {
    let key = text(input, "key");
    let done = match text(input, "op") {
        "get" => report(state::get(key).map(|value| value.map_or(Value::Null, Value::from))),
        "set" => report(state::set(key, text(input, "value")).map(|()| true)),
        "remove" => report(state::remove(key).map(|()| true)),
        _ => report(state::keys()),
    };
    if input.get("trap") == Some(&Value::Bool(true)) {
        std::process::abort();
    }
    done.to_string()
}

fn ask(input: &Value) -> String {
    let request = llm::Completion {
        system: text(input, "system").to_owned(),
        messages: vec![llm::Turn {
            role: llm::Role::User,
            text: text(input, "prompt").to_owned(),
        }],
    };
    report(llm::complete(text(input, "model"), &request)).to_string()
}

fn fetch(input: &Value) -> String {
    // Credentials must outlive the request that borrows them.
    let mut credentials = Vec::new();
    let mut named = Vec::new();
    for header in input
        .get("headers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = text(header, "name").to_owned();
        match header.get("secret").and_then(Value::as_str) {
            Some(secret_name) => match secret::get(secret_name) {
                Ok(credential) if credential.name() != secret_name => {
                    return json!({"failed": "the credential has another name"}).to_string();
                }
                Ok(credential) => {
                    credentials.push(credential);
                    named.push((name, None));
                }
                Err(error) => return report::<Value>(Err(error)).to_string(),
            },
            None => named.push((name, Some(text(header, "text").to_owned()))),
        }
    }
    let mut secrets = credentials.iter();
    let mut headers = Vec::new();
    for (name, value) in named {
        let value = match value {
            Some(text) => http::HeaderValue::Text(text),
            None => match secrets.next() {
                Some(credential) => http::HeaderValue::Secret(credential),
                None => continue,
            },
        };
        headers.push(http::Header { name, value });
    }
    let request = http::Request {
        method: text(input, "method").to_owned(),
        url: text(input, "url").to_owned(),
        headers,
        body: input
            .get("body")
            .and_then(Value::as_str)
            .map(|body| body.as_bytes().to_vec()),
    };
    report(http::send(&request).map(|response| {
        json!({
            "status": response.status,
            "body": String::from_utf8_lossy(&response.body),
        })
    }))
    .to_string()
}

fn read(input: &Value) -> String {
    let session = input.get("session").and_then(Value::as_str);
    let after = input
        .get("after")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .and_then(|limit| u32::try_from(limit).ok())
        .unwrap_or(u32::MAX);
    report(
        log_read::read(session, text(input, "topic"), after, limit).map(|events| {
            events
                .into_iter()
                .map(|event| json!([event.id, event.session, event.topic]))
                .collect::<Vec<_>>()
        }),
    )
    .to_string()
}

struct Testbed;

impl Plugin for Testbed {
    fn activate(config: String) -> Result<PluginInfo, String> {
        let config: Value = serde_json::from_str(&config).map_err(|error| error.to_string())?;
        let prefix = text(&config, "prefix").to_owned();
        let spec = |name: &str| {
            ToolSpec::new(
                &format!("{prefix}{name}"),
                &format!("The testbed's `{name}`."),
                r#"{"type":"object"}"#,
            )
        };
        let provides = strings(&config, "provides");
        let gates = strings(&config, "gates");
        let echo = config.get("echo").cloned().unwrap_or(Value::Null);
        let subscriptions = match text(&echo, "on") {
            "" => Vec::new(),
            topic => vec![topic.to_owned()],
        };
        let _ = ECHO.set(text(&echo, "emit").to_owned());
        let _ = ECHO_SESSION.set(echo.get("session").and_then(Value::as_bool) == Some(true));
        let tools = [
            "probe", "count", "trap", "relay", "state", "load", "unload", "loaded", "ask", "fetch",
            "read", "tools", "picture", "session",
        ]
        .map(spec)
        .to_vec();
        // A restarted instance is activated with the same configuration, so the same prefix.
        let _ = PREFIX.set(prefix);
        Ok(PluginInfo {
            tools,
            subscriptions,
            gates,
            provides,
            ..PluginInfo::new("testbed", env!("CARGO_PKG_VERSION"))
        })
    }

    fn call_tool(name: String, input: String) -> Result<String, ToolError> {
        let input: Value = serde_json::from_str(&input)
            .map_err(|error| ToolError::InvalidInput(error.to_string()))?;
        let prefix = PREFIX.get().map_or("", String::as_str);
        match name.strip_prefix(prefix).unwrap_or(&name) {
            "probe" => Ok(probe()),
            "count" => Ok((COUNT.fetch_add(1, Ordering::SeqCst) + 1).to_string()),
            // On WebAssembly an abort is the `unreachable` instruction: a trap.
            "trap" => std::process::abort(),
            "relay" => Ok(relay(&input)),
            "state" => Ok(store(&input)),
            "load" => Ok(load(&input)),
            "unload" => {
                Ok(report(plugins::unload(text(&input, "name")).map(|()| true)).to_string())
            }
            "loaded" => Ok(json!(plugins::loaded()).to_string()),
            "ask" => Ok(ask(&input)),
            "fetch" => Ok(fetch(&input)),
            "read" => Ok(read(&input)),
            "session" => Ok(json!({
                "current": report(session::current().map(|current| json!(current))),
                "call": report(session::call().map(|call| json!(call))),
                "stopping": session::stopping(),
            })
            .to_string()),
            "tools" => Ok(json!(
                tools::list()
                    .into_iter()
                    .map(|spec| spec.name)
                    .collect::<Vec<_>>()
            )
            .to_string()),
            _ => Err(ToolError::UnknownTool(name)),
        }
    }

    fn call_tool_content(name: String, input: String) -> Result<Vec<Content>, ToolError> {
        let prefix = PREFIX.get().map_or("", String::as_str);
        if name.strip_prefix(prefix).unwrap_or(&name) != "picture" {
            // Every other tool returns text.
            return Self::call_tool(name, input).map(|text| vec![Content::text(text)]);
        }
        let input: Value = serde_json::from_str(&input)
            .map_err(|error| ToolError::InvalidInput(error.to_string()))?;
        Ok(picture(&input))
    }

    fn on_event(event: Event) {
        if let Some(topic) = ECHO.get().filter(|topic| !topic.is_empty()) {
            let payload = if ECHO_SESSION.get() == Some(&true) {
                json!({"session": report(session::current().map(|current| json!(current)))})
                    .to_string()
            } else {
                event.payload
            };
            // Refused or not, there is nobody to tell from inside a listener.
            let _ = events::emit(topic, &payload);
        }
    }

    fn handle_call(call: ServiceCall) -> Result<String, CallError> {
        match call.method.as_str() {
            "echo" => Ok(json!({
                "caller": call.caller,
                "service": call.service,
                "payload": call.payload,
            })
            .to_string()),
            "relay" => {
                let input: Value = serde_json::from_str(&call.payload)
                    .map_err(|error| CallError::InvalidInput(error.to_string()))?;
                Ok(relay(&input))
            }
            method => Err(CallError::Unknown(format!("method `{method}`"))),
        }
    }
}

corduroy_guest::export!(Testbed);
