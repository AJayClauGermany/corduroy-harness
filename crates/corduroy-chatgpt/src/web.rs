//! The few HTTP requests a sign-in makes, and the encoding of what goes in a URL.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use corduroy_core::{HttpClient, HttpRequest};
use serde_json::Value;

/// How long one request to the sign-in service may take.
const WAIT: Duration = Duration::from_secs(30);

/// What this tool calls itself to the sign-in service.
const USER_AGENT: &str = concat!("corduroy/", env!("CARGO_PKG_VERSION"));

/// An answer: its status, and its body if that was JSON.
#[derive(Debug, Clone)]
pub struct Reply {
    /// The status code.
    pub status: u16,
    /// The body, or `Null` where it was not JSON.
    pub body: Value,
}

impl Reply {
    /// Whether the status is one of success.
    #[must_use]
    pub const fn ok(&self) -> bool {
        self.status >= 200 && self.status < 300
    }

    /// The machine-readable code of an error answer, wherever the service put it: `error`
    /// as text (OAuth), `error.code`, or `code`.
    #[must_use]
    pub fn code(&self) -> &str {
        let error = self.body.get("error");
        error
            .and_then(Value::as_str)
            .or_else(|| {
                error
                    .and_then(|error| error.get("code"))
                    .and_then(Value::as_str)
            })
            .or_else(|| self.body.get("code").and_then(Value::as_str))
            .unwrap_or("")
    }

    /// The answer as a reason something failed: the status and the code, never the body,
    /// which may repeat what was sent.
    #[must_use]
    pub fn refusal(&self) -> String {
        match self.code() {
            "" => format!("HTTP {}", self.status),
            code => format!("HTTP {} ({code})", self.status),
        }
    }
}

/// The requests of a sign-in, through one client.
#[derive(Clone)]
pub struct Web {
    client: Arc<dyn HttpClient>,
}

impl std::fmt::Debug for Web {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Web").finish_non_exhaustive()
    }
}

impl Web {
    /// Requests sent through `client`.
    #[must_use]
    pub fn new(client: Arc<dyn HttpClient>) -> Self {
        Self { client }
    }

    fn send(&self, request: &HttpRequest) -> Result<Reply, String> {
        let response = self
            .client
            .send(request, WAIT)
            .map_err(|why| format!("{}: {why}", origin(&request.url)))?;
        Ok(Reply {
            status: response.status,
            body: serde_json::from_slice(&response.body).unwrap_or(Value::Null),
        })
    }

    /// The JSON document at `url`.
    ///
    /// # Errors
    ///
    /// If the request fails or the answer is not a success.
    pub fn get(&self, url: &str) -> Result<Value, String> {
        let reply = self.send(&HttpRequest {
            method: "GET".to_owned(),
            url: url.to_owned(),
            headers: vec![
                ("Accept".to_owned(), "application/json".to_owned()),
                ("User-Agent".to_owned(), USER_AGENT.to_owned()),
            ],
            body: None,
        })?;
        if reply.ok() {
            Ok(reply.body)
        } else {
            Err(format!("{}: {}", origin(url), reply.refusal()))
        }
    }

    /// Post `form` to `url`. An answer with an error status is still an answer.
    ///
    /// # Errors
    ///
    /// If the request fails.
    pub fn post(&self, url: &str, form: &[(&str, &str)]) -> Result<Reply, String> {
        self.send(&HttpRequest {
            method: "POST".to_owned(),
            url: url.to_owned(),
            headers: vec![
                ("Accept".to_owned(), "application/json".to_owned()),
                (
                    "Content-Type".to_owned(),
                    "application/x-www-form-urlencoded".to_owned(),
                ),
                ("User-Agent".to_owned(), USER_AGENT.to_owned()),
            ],
            body: Some(query(form).into_bytes()),
        })
    }
}

/// The scheme and host of `url`: what an error may say about where it went.
fn origin(url: &str) -> &str {
    let after = url.find("://").map_or(0, |at| at + 3);
    let end = url
        .char_indices()
        .skip(after)
        .find(|(_, character)| matches!(character, '/' | '?' | '#'))
        .map_or(url.len(), |(at, _)| at);
    url.get(..end).unwrap_or(url)
}

/// `text` percent-encoded: everything but RFC 3986's unreserved characters.
#[must_use]
pub fn encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            let _written = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// `pairs` as a query string, or a form body.
#[must_use]
pub fn query(pairs: &[(&str, &str)]) -> String {
    let pairs: Vec<String> = pairs
        .iter()
        .map(|(name, value)| format!("{}={}", encode(name), encode(value)))
        .collect();
    pairs.join("&")
}

/// `text` from a query string: percent-decoded, with `+` a space.
#[must_use]
pub fn decode(text: &str) -> String {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.bytes();
    while let Some(byte) = rest.next() {
        match byte {
            b'+' => bytes.push(b' '),
            b'%' => {
                let pair = [rest.next(), rest.next()];
                let value = match pair {
                    [Some(high), Some(low)] => std::str::from_utf8(&[high, low])
                        .ok()
                        .and_then(|digits| u8::from_str_radix(digits, 16).ok()),
                    _ => None,
                };
                // Something that is not an escape is kept as it came.
                if let Some(value) = value {
                    bytes.push(value);
                } else {
                    bytes.push(b'%');
                    bytes.extend(pair.into_iter().flatten());
                }
            }
            other => bytes.push(other),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn what_goes_in_a_url_is_encoded_and_comes_back() {
        let scope = "openid offline_access";
        assert_eq!(encode(scope), "openid%20offline_access");
        assert_eq!(
            query(&[
                ("redirect_uri", "http://127.0.0.1:1455/auth/callback"),
                ("a", "b&c")
            ]),
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback&a=b%26c"
        );
        assert_eq!(
            decode("chatgpt.tokens.use.direct+email%20x"),
            "chatgpt.tokens.use.direct email x"
        );
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode(&encode("a/b ü?")), "a/b ü?");
    }

    #[test]
    fn an_error_says_where_and_which_code_and_never_the_body() {
        assert_eq!(
            origin("https://auth.openai.com/api/x?token=abc"),
            "https://auth.openai.com"
        );
        let reply = |body: Value| Reply { status: 400, body };
        assert_eq!(
            reply(json!({"error": "invalid_grant"})).code(),
            "invalid_grant"
        );
        assert_eq!(
            reply(json!({"error": {"code": "token_expired"}})).code(),
            "token_expired"
        );
        assert_eq!(reply(json!({"code": "x"})).code(), "x");
        let refused = reply(json!({"error": "invalid_grant", "error_description": "tok3n"}));
        assert_eq!(refused.refusal(), "HTTP 400 (invalid_grant)");
    }
}
