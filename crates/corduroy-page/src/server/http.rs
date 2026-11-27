//! Just enough HTTP for one page on the loopback interface: a request read whole, a
//! response written whole, and the connection closed.
//!
//! No keep-alive, no chunked bodies, no TLS. What does not fit is refused, and a request
//! larger than a page ever sends is not read to its end.

use std::io::{self, Read, Write};

/// The most a request's line and headers may take.
const MAX_HEAD: usize = 16 * 1024;
/// The most a request's body may take: an answer is a few hundred bytes.
pub(super) const MAX_BODY: usize = 64 * 1024;

/// A request, read whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Request {
    /// `GET`, `POST` and so on, as sent.
    pub(super) method: String,
    /// The path asked for, with its query if it has one.
    pub(super) target: String,
    /// The headers, names in lowercase.
    pub(super) headers: Vec<(String, String)>,
    /// The body, empty if there is none.
    pub(super) body: Vec<u8>,
}

impl Request {
    /// The value of the header `name` (in lowercase), if it was sent.
    #[must_use]
    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }

    /// The path without its query.
    #[must_use]
    pub(super) fn path(&self) -> &str {
        self.target.split(['?', '#']).next().unwrap_or_default()
    }
}

/// Why a request was not read.
#[derive(Debug)]
pub(super) enum Unread {
    /// It is not a request this server understands; the status to answer with, and why.
    Bad(u16, &'static str),
    /// The connection failed or went quiet before the request was whole.
    Gone(io::Error),
}

impl From<io::Error> for Unread {
    fn from(error: io::Error) -> Self {
        Self::Gone(error)
    }
}

/// Where the head of a request ends in `bytes`: just past its blank line.
fn head_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| at + 4)
}

/// Read one request from `stream`.
///
/// # Errors
///
/// [`Unread::Bad`] with the status to answer if it is too large or not well formed, or
/// [`Unread::Gone`] if the connection gave out first.
pub(super) fn read(stream: &mut impl Read) -> Result<Request, Unread> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 2048];
    let end = loop {
        if let Some(end) = head_end(&bytes) {
            break end;
        }
        if bytes.len() > MAX_HEAD {
            return Err(Unread::Bad(431, "the request's headers are too large"));
        }
        let read = stream.read(&mut chunk)?;
        let Some(received) = chunk.get(..read).filter(|received| !received.is_empty()) else {
            return Err(Unread::Bad(400, "the request ended before its headers did"));
        };
        bytes.extend_from_slice(received);
    };
    let (head, rest) = bytes
        .split_at_checked(end)
        .ok_or(Unread::Bad(400, "the request is not well formed"))?;
    let mut request = parse(head)?;
    let length = match request.header("content-length") {
        Some(length) => length
            .trim()
            .parse::<usize>()
            .map_err(|_| Unread::Bad(400, "the request's length is not a number"))?,
        None if request.header("transfer-encoding").is_some() => {
            return Err(Unread::Bad(411, "the request has to say its length"));
        }
        None => 0,
    };
    if length > MAX_BODY {
        return Err(Unread::Bad(413, "the request is too large"));
    }
    let mut body = rest.to_vec();
    body.truncate(length);
    let missing = length - body.len();
    if missing > 0 {
        let mut more = vec![0_u8; missing];
        stream.read_exact(&mut more)?;
        body.extend_from_slice(&more);
    }
    request.body = body;
    Ok(request)
}

/// The request line and headers in `head`.
fn parse(head: &[u8]) -> Result<Request, Unread> {
    let bad = Unread::Bad(400, "the request is not well formed");
    let Ok(text) = std::str::from_utf8(head) else {
        return Err(bad);
    };
    let mut lines = text.split("\r\n");
    let mut line = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (line.next(), line.next(), line.next(), line.next())
    else {
        return Err(bad);
    };
    if !version.starts_with("HTTP/1.") || !target.starts_with('/') {
        return Err(bad);
    }
    let headers = lines
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect::<Option<Vec<_>>>()
        .ok_or(bad)?;
    Ok(Request {
        method: method.to_owned(),
        target: target.to_owned(),
        headers,
        body: Vec::new(),
    })
}

/// A response, written whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Response {
    /// The status code.
    pub(super) status: u16,
    /// What kind of thing the body is, such as `text/html; charset=utf-8`.
    pub(super) kind: &'static str,
    /// The body.
    pub(super) body: Vec<u8>,
    /// Where to go instead, for a redirect.
    pub(super) location: Option<String>,
}

impl Response {
    /// A response of `status` whose body is `body`, of the kind `kind`.
    #[must_use]
    pub(super) fn new(status: u16, kind: &'static str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            kind,
            body: body.into(),
            location: None,
        }
    }

    /// A response of `status` that says `said` in plain words.
    #[must_use]
    pub(super) fn plain(status: u16, said: &str) -> Self {
        Self::new(status, "text/plain; charset=utf-8", format!("{said}\n"))
    }

    /// A response of `status` whose body is `value` as JSON.
    #[must_use]
    pub(super) fn json(status: u16, value: &serde_json::Value) -> Self {
        Self::new(status, "application/json", value.to_string())
    }

    /// A redirect to `location`.
    #[must_use]
    pub(super) fn to(location: String) -> Self {
        Self {
            location: Some(location),
            ..Self::plain(301, "moved")
        }
    }

    /// Write the response to `stream`, with the headers every response of the page carries:
    /// nothing is cached, nothing is told to another site, the body is what it says it is,
    /// and the page loads and reaches only what is served beside it.
    ///
    /// # Errors
    ///
    /// If `stream` cannot be written to.
    pub(super) fn write(&self, stream: &mut impl Write) -> io::Result<()> {
        let reason = match self.status {
            200 => "OK",
            301 => "Moved Permanently",
            400 => "Bad Request",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            409 => "Conflict",
            411 => "Length Required",
            413 => "Content Too Large",
            431 => "Request Header Fields Too Large",
            _ => "Internal Server Error",
        };
        let mut head = format!(
            "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
             Connection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\n\
             X-Content-Type-Options: nosniff\r\nContent-Security-Policy: {POLICY}\r\n",
            self.status,
            self.kind,
            self.body.len(),
        );
        if let Some(location) = &self.location {
            head.push_str("Location: ");
            head.push_str(location);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes())?;
        stream.write_all(&self.body)?;
        stream.flush()
    }
}

/// What the page may load and reach: its own files, and the address it was served from.
pub(super) const POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
                          connect-src 'self'; base-uri 'none'; form-action 'none'; \
                          frame-ancestors 'none'";

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn request(text: &str) -> Result<Request, Unread> {
        read(&mut Cursor::new(text.as_bytes().to_vec()))
    }

    #[test]
    fn a_request_is_read_with_its_headers_and_body() {
        let read = request(
            "POST /t/answer?x=1 HTTP/1.1\r\nHost: 127.0.0.1:9\r\nContent-Type: application/json\r\n\
             Content-Length: 11\r\n\r\n{\"ok\":true}trailing",
        )
        .unwrap();
        assert_eq!(read.method, "POST");
        assert_eq!(read.target, "/t/answer?x=1");
        assert_eq!(read.path(), "/t/answer");
        assert_eq!(read.header("host"), Some("127.0.0.1:9"));
        assert_eq!(read.header("content-type"), Some("application/json"));
        assert_eq!(read.body, b"{\"ok\":true}");
        let plain = request("GET / HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!((plain.path(), plain.body.len()), ("/", 0));
    }

    #[test]
    fn what_does_not_fit_or_is_not_a_request_is_refused_with_a_status() {
        let status = |text: &str| match request(text) {
            Err(Unread::Bad(status, _)) => status,
            other => panic!("{other:?}"),
        };
        assert_eq!(status("nonsense\r\n\r\n"), 400);
        assert_eq!(status("GET http://x/ HTTP/1.1\r\n\r\n"), 400);
        assert_eq!(status("GET / HTTP/1.1\r\nno colon\r\n\r\n"), 400);
        assert_eq!(status("GET / HTTP/1.1\r\nHost: x"), 400);
        assert_eq!(
            status("POST / HTTP/1.1\r\nContent-Length: many\r\n\r\n"),
            400
        );
        assert_eq!(
            status("POST / HTTP/1.1\r\nContent-Length: 999999\r\n\r\n"),
            413
        );
        assert_eq!(
            status("POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"),
            411
        );
        let long = format!(
            "GET / HTTP/1.1\r\nX: {}\r\n\r\n",
            "a".repeat(MAX_HEAD + 4096)
        );
        assert_eq!(status(&long), 431);
        // A body that never arrives is the connection's failure, not a status.
        let short = request("POST / HTTP/1.1\r\nContent-Length: 5\r\n\r\nab");
        assert!(matches!(short, Err(Unread::Gone(_))), "{short:?}");
    }

    #[test]
    fn a_response_carries_its_length_and_the_headers_that_keep_the_page_to_itself() {
        let mut written = Vec::new();
        Response::json(409, &serde_json::json!({"error": "no"}))
            .write(&mut written)
            .unwrap();
        let text = String::from_utf8(written).unwrap();
        assert!(text.starts_with("HTTP/1.1 409 Conflict\r\n"), "{text}");
        assert!(text.contains("Content-Length: 14\r\n"), "{text}");
        assert!(text.contains("Cache-Control: no-store\r\n"), "{text}");
        assert!(text.contains("Referrer-Policy: no-referrer\r\n"), "{text}");
        assert!(text.contains("connect-src 'self'"), "{text}");
        assert!(text.ends_with("\r\n\r\n{\"error\":\"no\"}"), "{text}");
        let mut moved = Vec::new();
        Response::to("/t/".to_owned()).write(&mut moved).unwrap();
        assert!(
            String::from_utf8(moved)
                .unwrap()
                .contains("Location: /t/\r\n")
        );
    }
}
