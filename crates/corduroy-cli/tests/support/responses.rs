//! A fake Responses endpoint on the loopback interface, which keeps what it was sent.

use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};

/// One request as the endpoint saw it: its `Authorization` header and its body.
#[derive(Debug, Clone)]
pub(crate) struct Sent {
    /// The `Authorization` header, whole.
    pub(crate) authorization: String,
    /// The body, as JSON.
    pub(crate) body: Value,
}

/// Read one request off `stream`.
pub(crate) fn read(stream: &TcpStream) -> Sent {
    let mut reader = BufReader::new(stream);
    let (mut length, mut authorization) = (0, String::new());
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
        let lower = line.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("authorization:") {
            authorization = line
                .split_once(':')
                .map(|(_, value)| value.trim().to_owned())
                .unwrap_or_default();
        }
        line.clear();
    }
    let mut body = vec![0; length];
    let _read = reader.read_exact(&mut body);
    Sent {
        authorization,
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    }
}

/// A Responses stream answering with `text`.
fn stream_of(text: &str) -> String {
    let item = json!({
        "type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
        "content": [{"type": "output_text", "text": text, "annotations": []}],
    });
    let response = |status: &str, output: Value| {
        json!({
            "id": "resp_1", "object": "response", "status": status, "model": "gpt-6-fake",
            "output": output,
            "usage": {"input_tokens": 5, "output_tokens": 2, "total_tokens": 7},
        })
    };
    let events = [
        (
            "response.created",
            json!({"response": response("in_progress", json!([]))}),
        ),
        (
            "response.output_item.added",
            json!({"output_index": 0, "item": item}),
        ),
        (
            "response.output_text.delta",
            json!({"item_id": "msg_1", "output_index": 0, "content_index": 0, "delta": text}),
        ),
        (
            "response.output_item.done",
            json!({"output_index": 0, "item": item}),
        ),
        (
            "response.completed",
            json!({"response": response("completed", json!([item]))}),
        ),
    ];
    let mut stream = String::new();
    for (number, (kind, mut event)) in events.into_iter().enumerate() {
        if let Value::Object(fields) = &mut event {
            fields.insert("type".to_owned(), json!(kind));
            fields.insert("sequence_number".to_owned(), json!(number));
        }
        let _written = write!(stream, "event: {kind}\ndata: {event}\n\n");
    }
    stream
}

/// The requests an endpoint received, in order.
pub(crate) type Seen = Arc<Mutex<Vec<Sent>>>;

/// A Responses endpoint that answers every request with `text`, and keeps each request.
pub(crate) fn endpoint(text: &'static str) -> std::io::Result<(SocketAddr, Seen)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&seen);
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let sent = read(&stream);
            if let Ok(mut seen) = kept.lock() {
                seen.push(sent);
            }
            let body = stream_of(text);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
            let _written = (&stream).write_all(response.as_bytes());
        }
    });
    Ok((address, seen))
}
