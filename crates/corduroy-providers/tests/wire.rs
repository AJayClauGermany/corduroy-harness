//! What a built provider puts on the wire, and how long it waits, against a server on
//! loopback that speaks just enough of `OpenAI`'s chat completions. No network and no model.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use corduroy_harness::core::{EventStream, Message, ModelInfo, ModelRequest, Pattern};
use corduroy_harness::{Harness, ProviderErrorKind, Status, provider};
use corduroy_providers::{ProviderSpec, build};
use serde_json::{Value, json};

/// The JSON body of the request on `stream`.
fn body_of(stream: &TcpStream) -> io::Result<Value> {
    let mut reader = BufReader::new(stream);
    let mut length = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body).unwrap_or(Value::Null))
}

/// A spec for the endpoint at `port`, asked without streaming unless `streaming`.
fn spec(port: u16, streaming: bool) -> ProviderSpec {
    ProviderSpec {
        base_url: Some(format!("http://127.0.0.1:{port}/v1/chat/completions")),
        streaming: Some(streaming),
        ..ProviderSpec::new("openai-compatible")
    }
}

/// An endpoint that answers each request with one short message, and hands on each
/// request's body. Its port, and the bodies as they come.
fn answering() -> io::Result<(u16, Receiver<Value>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let (seen, bodies) = mpsc::channel();
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let Ok(body) = body_of(&stream) else { continue };
            if seen.send(body).is_err() {
                return;
            }
            let answer = json!({
                "id": "1", "object": "chat.completion", "created": 0, "model": "local",
                "choices": [{
                    "index": 0, "finish_reason": "stop",
                    "message": {"role": "assistant", "content": "hello"},
                }],
                "usage": {"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4},
            })
            .to_string();
            let _sent = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{answer}",
                answer.len()
            );
        }
    });
    Ok((port, bodies))
}

/// An endpoint whose answer never ends: a piece of a stream every 50 ms for as long as
/// anyone is reading. Its port, and a word when a connection it was writing to has closed.
fn streaming_forever() -> io::Result<(u16, Receiver<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let (closed, hung_up) = mpsc::channel();
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let closed = closed.clone();
            thread::spawn(move || {
                let _ended = sing(&mut stream);
                let _told = closed.send(());
            });
        }
    });
    Ok((port, hung_up))
}

/// Read the request on `stream` and answer it with a stream that goes on until nobody is
/// reading: the error that ends it.
fn sing(stream: &mut TcpStream) -> io::Error {
    let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
    if let Err(error) = body_of(stream).and_then(|_| stream.write_all(head.as_bytes())) {
        return error;
    }
    let piece = json!({
        "id": "1", "object": "chat.completion.chunk", "created": 0, "model": "local",
        "choices": [{"index": 0, "delta": {"role": "assistant", "content": "la "}}],
    });
    let line = format!("data: {piece}\n\n");
    loop {
        if let Err(error) = stream
            .write_all(line.as_bytes())
            .and_then(|()| stream.flush())
        {
            return error;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// An endpoint that refuses its first `refused` requests as coming too fast, to be tried
/// again at once, and answers the rest. Its port.
fn refusing_first(refused: usize) -> io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    thread::spawn(move || {
        for (count, mut stream) in listener.incoming().flatten().enumerate() {
            if body_of(&stream).is_err() {
                continue;
            }
            let (status, answer) = if count < refused {
                (
                    "429 Too Many Requests\r\nretry-after: 0",
                    json!({"error": {"message": "slow down", "type": "rate_limit_error"}}),
                )
            } else {
                (
                    "200 OK",
                    json!({
                        "id": "1", "object": "chat.completion", "created": 0, "model": "local",
                        "choices": [{
                            "index": 0, "finish_reason": "stop",
                            "message": {"role": "assistant", "content": "hello"},
                        }],
                        "usage": {"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4},
                    }),
                )
            };
            let answer = answer.to_string();
            let _sent = write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{answer}",
                answer.len()
            );
        }
    });
    Ok(port)
}

fn asking() -> ModelRequest {
    ModelRequest::new("local", "be brief", vec![Message::user().text("hi")])
}

#[tokio::test(flavor = "multi_thread")]
async fn a_model_nobody_describes_is_still_sent_a_limit() {
    let (port, bodies) = answering().unwrap();
    let wait = Duration::from_secs(5);

    // Unknown to the catalog and to the host: 4096.
    let plain = build("local", &spec(port, false)).unwrap();
    plain.complete(&asking()).await.unwrap();
    assert_eq!(bodies.recv_timeout(wait).unwrap()["max_tokens"], 4096);

    // A quarter of the context the host states.
    let mut info = ModelInfo::unknown();
    info.context_tokens = Some(32_768);
    let stated = ProviderSpec {
        models: BTreeMap::from([("local".to_owned(), info)]),
        ..spec(port, false)
    };
    let stated = build("local", &stated).unwrap();
    stated.complete(&asking()).await.unwrap();
    assert_eq!(bodies.recv_timeout(wait).unwrap()["max_tokens"], 8192);

    // What the entry sets is what is sent.
    let set = ProviderSpec {
        max_tokens: Some(500),
        ..spec(port, false)
    };
    let set = build("local", &set).unwrap();
    set.complete(&asking()).await.unwrap();
    assert_eq!(bodies.recv_timeout(wait).unwrap()["max_tokens"], 500);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_that_never_ends_is_ended_at_the_timeout() {
    let (port, hung_up) = streaming_forever().unwrap();
    let endless = ProviderSpec {
        timeout_seconds: Some(1),
        ..spec(port, true)
    };
    let built = build("local", &endless).unwrap();

    // The provider says which sort of failure it was.
    let began = Instant::now();
    let error = built.complete(&asking()).await.unwrap_err();
    let took = began.elapsed();
    assert_eq!(error.kind(), ProviderErrorKind::Timeout, "{error}");
    assert!(error.to_string().contains("timeout_seconds"), "{error}");
    assert!(
        took >= Duration::from_secs(1) && took < Duration::from_secs(5),
        "one attempt, ended at its bound: took {took:?}"
    );
    // Dropping the attempt closed its connection.
    hung_up.recv_timeout(Duration::from_secs(5)).unwrap();

    // And a run that asked ends as failed, with the kind recorded beside the reason.
    let events = Arc::new(EventStream::new());
    let harness = Harness::open(Arc::clone(&events))
        .unwrap()
        .with_model("local");
    harness.plugin(provider("default", built));
    let outcome = harness.run("sing").await.unwrap();
    assert!(
        matches!(outcome.status, Status::Failed { .. }),
        "{:?}",
        outcome.status
    );
    let ends = events.matching(&Pattern::new("turn/end"));
    let end: Value = serde_json::from_str(&ends.last().unwrap().payload).unwrap();
    assert_eq!(end["status"], "failed");
    assert_eq!(end["kind"], "timeout");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_made_again_is_recorded_with_who_retried() {
    // oi-kit's own client tries a refused request four times before it gives the error up;
    // the fifth request is the retry made here, and the one that is told.
    let port = refusing_first(4).unwrap();
    let built = build("local", &spec(port, false)).unwrap();
    let events = Arc::new(EventStream::new());
    let harness = Harness::open(Arc::clone(&events))
        .unwrap()
        .with_model("local");
    harness.plugin(provider("default", built));

    let outcome = harness.run("hi").await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "hello".to_owned()
        }
    );
    let retries = events.matching(&Pattern::new("model/retry"));
    assert_eq!(retries.len(), 1, "{retries:?}");
    let retry: Value = serde_json::from_str(&retries[0].payload).unwrap();
    assert_eq!(retry["attempt"], 1);
    assert_eq!(retry["kind"], "rate-limit");
    assert_eq!(retry["by"], "provider");
    assert_eq!(retry["provider"], "default");
    assert_eq!(retry["model"], "local");
    assert!(retry["delay_ms"].is_u64(), "{retry}");
    assert!(retry["reason"].is_string(), "{retry}");
    // Between the request and its answer, in the session that asked.
    let stored = events.events();
    let topics: Vec<&str> = stored
        .iter()
        .map(|event| event.topic.as_str())
        .filter(|topic| topic.starts_with("model/") || *topic == "usage")
        .collect();
    assert_eq!(topics, ["model/request", "model/retry", "usage"]);
}
