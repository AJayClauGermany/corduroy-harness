//! The client against a server on the loopback interface: no network needed.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use corduroy_core::{HttpClient, HttpRequest};
use corduroy_http::UreqClient;

/// The request line, headers and body of one request, as text.
fn read_request(stream: &TcpStream) -> String {
    let mut reader = BufReader::new(stream);
    let mut head = String::new();
    let mut length = 0;
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
        head.push_str(&line);
        line.clear();
    }
    let mut body = vec![0; length];
    let _read = reader.read_exact(&mut body);
    head.push_str(&String::from_utf8_lossy(&body));
    head
}

/// A server that answers `count` requests, each with `reply(request text)`.
fn serve(
    count: usize,
    reply: impl Fn(&str) -> String + Send + 'static,
) -> std::io::Result<SocketAddr> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    thread::spawn(move || {
        for stream in listener.incoming().take(count).flatten() {
            let answer = reply(&read_request(&stream));
            let _written = (&stream).write_all(answer.as_bytes());
        }
    });
    Ok(address)
}

fn request(method: &str, url: String, headers: &[(&str, &str)], body: Option<&str>) -> HttpRequest {
    HttpRequest {
        method: method.into(),
        url,
        headers: headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        body: body.map(|body| body.as_bytes().to_vec()),
    }
}

#[test]
fn a_request_goes_out_with_its_headers_and_body() {
    let address = serve(1, |head| {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{head}",
            head.len()
        )
    })
    .unwrap();
    let response = UreqClient::new()
        .send(
            &request(
                "POST",
                format!("http://{address}/echo"),
                &[("authorization", "Bearer s3cret")],
                Some("hello"),
            ),
            Duration::from_secs(5),
        )
        .unwrap();
    let echoed = String::from_utf8(response.body).unwrap();
    assert_eq!(response.status, 200);
    assert!(echoed.starts_with("POST /echo HTTP/1.1"), "{echoed}");
    assert!(echoed.contains("authorization: Bearer s3cret"), "{echoed}");
    assert!(echoed.ends_with("hello"), "{echoed}");
    assert!(
        response
            .headers
            .iter()
            .any(|(name, value)| name == "content-type" && value == "text/plain")
    );
}

/// A redirect elsewhere would go around the host check, so it comes back as a response.
#[test]
fn a_redirect_is_returned_not_followed() {
    let address = serve(1, |_| {
        "HTTP/1.1 302 Found\r\nLocation: http://elsewhere.invalid/\r\nContent-Length: 0\r\n\r\n"
            .to_owned()
    })
    .unwrap();
    let response = UreqClient::new()
        .send(
            &request("GET", format!("http://{address}/"), &[], None),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(response.status, 302);
    assert!(
        response
            .headers
            .iter()
            .any(|(name, value)| name == "location" && value == "http://elsewhere.invalid/")
    );
}

#[test]
fn an_error_status_is_a_response_and_no_answer_is_an_error() {
    let address = serve(1, |_| {
        "HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\n\r\ngone".to_owned()
    })
    .unwrap();
    let client = UreqClient::new();
    let response = client
        .send(
            &request("GET", format!("http://{address}/"), &[], None),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(
        (response.status, response.body.as_slice()),
        (404, &b"gone"[..])
    );

    // Nothing listens on a port that was just released.
    let closed = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let refused = client.send(
        &request("GET", format!("http://{closed}/"), &[], None),
        Duration::from_secs(5),
    );
    assert!(refused.is_err());
}
