//! Read a web page as text: `web_fetch`, through the `http` capability.
//!
//! Documentation, an issue, a changelog: much of what a coding agent needs to know is on a
//! page somewhere. This plugin fetches one address through the host's HTTP client, within
//! the hosts its `http` grant names, and hands the model what the page says: HTML becomes
//! text with its headings, lists and links kept ([`html`]), and text and JSON come as they
//! are.
//!
//! The host's client follows no redirects, because it checked one host against the grant.
//! This plugin follows them itself, each as a request of its own, so a redirect to a host
//! outside the grant is refused like any other request there.
//!
//! The tool is not marked read-only. It changes nothing here, but an address is something
//! sent out, and a model that has read a hostile page can be told to put what it knows in
//! one. A gate that asks about tools that are not read-only asks about each fetch.
//!
//! Configuration: `max_bytes`, the most text one call returns (50 000 by default; the rest
//! is reached with `offset`), and `user_agent`.

mod html;
mod url;

use std::fmt::Write as _;
use std::sync::OnceLock;

use corduroy_guest::host::http::{self, Header, HeaderValue, Request, Response};
use corduroy_guest::{HostError, Plugin, PluginInfo, ToolError, ToolSpec, parse};
use serde::Deserialize;

/// Redirects followed for one fetch, at most.
const MAX_REDIRECTS: usize = 5;
/// Characters of a failed response shown with its status.
const MAX_REFUSAL: usize = 500;

/// Plugin configuration, from the host.
#[derive(Deserialize)]
#[serde(default)]
struct Config {
    /// Bytes of text one call returns at most.
    max_bytes: usize,
    /// What the requests say they come from.
    user_agent: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_bytes: 50_000,
            user_agent: concat!("corduroy-fetch/", env!("CARGO_PKG_VERSION")).to_owned(),
        }
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();

#[derive(Deserialize)]
struct Fetch {
    url: String,
    /// Bytes of the text to pass over, to read on from where an earlier call stopped.
    #[serde(default)]
    offset: usize,
    /// The page as it was sent, markup and all.
    #[serde(default)]
    raw: bool,
}

/// What kind of thing a response's body is.
#[derive(Debug, PartialEq, Eq)]
enum Kind {
    Html,
    Text,
    /// Not text: an image, an archive, a PDF.
    Other,
}

/// The value of the header `name` in `response`.
fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response
        .headers
        .iter()
        .find(|(header, _)| header.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// What `body` is, going by the `content_type` it was sent with, and by how it begins where
/// that says nothing.
fn kind(content_type: Option<&str>, body: &[u8]) -> Kind {
    let declared = content_type.unwrap_or_default().to_ascii_lowercase();
    if declared.contains("html") {
        return Kind::Html;
    }
    let textual = ["text/", "json", "xml", "javascript", "yaml", "toml", "csv"];
    if textual.iter().any(|part| declared.contains(part)) {
        return Kind::Text;
    }
    if !declared.is_empty() && !declared.contains("octet-stream") {
        return Kind::Other;
    }
    let start = String::from_utf8_lossy(body.get(..512).unwrap_or(body)).to_ascii_lowercase();
    let start = start.trim_start();
    if start.starts_with("<!doctype html") || start.starts_with("<html") {
        Kind::Html
    } else if std::str::from_utf8(body).is_ok() {
        Kind::Text
    } else {
        Kind::Other
    }
}

/// At most `max` bytes of `text` from `offset` on, on whole characters, and the offset to
/// read on from if that is not all of it.
fn window(text: &str, offset: usize, max: usize) -> (&str, Option<usize>) {
    let from = text.floor_char_boundary(offset.min(text.len()));
    let to = text.floor_char_boundary(from.saturating_add(max.max(4)).min(text.len()));
    let shown = text.get(from..to).unwrap_or_default();
    (shown, (to < text.len()).then_some(to))
}

/// What to tell the model about `response`, fetched from `address` after `hops` redirects.
fn render(
    address: &str,
    hops: usize,
    response: &Response,
    input: &Fetch,
    config: &Config,
) -> Result<String, ToolError> {
    let content_type = header(response, "content-type");
    let kind = kind(content_type, &response.body);
    let body = String::from_utf8_lossy(&response.body);
    let text = match kind {
        Kind::Other => String::new(),
        Kind::Html if !input.raw => html::to_text(&body, address),
        Kind::Html | Kind::Text => body.into_owned(),
    };
    if response.status >= 400 {
        let said: String = text.chars().take(MAX_REFUSAL).collect();
        return Err(ToolError::Failed(format!(
            "{address} answered {}{}",
            response.status,
            if said.trim().is_empty() {
                String::new()
            } else {
                format!(":\n{}", said.trim())
            }
        )));
    }
    if kind == Kind::Other {
        return Ok(format!(
            "{address} is {}, {} bytes: not text, so it is not shown.",
            content_type.unwrap_or("of no stated kind"),
            response.body.len()
        ));
    }
    let (shown, more) = window(&text, input.offset, config.max_bytes);
    let mut said = String::new();
    if hops > 0 {
        // Writing to a `String` cannot fail.
        let _ = writeln!(said, "[redirected to {address}]");
    }
    if shown.trim().is_empty() {
        said.push_str(if input.offset > 0 {
            "[nothing more: the page ends before that offset]"
        } else {
            "[the page has no text: it may be built by scripts, which are not run]"
        });
    } else {
        said.push_str(shown);
    }
    if let Some(next) = more {
        let _ = write!(
            said,
            "\n[{} more bytes: call again with offset {next}]",
            text.len() - next
        );
    }
    Ok(said)
}

fn fetch(input: &Fetch) -> Result<String, ToolError> {
    let fallback = Config::default();
    let config = CONFIG.get().unwrap_or(&fallback);
    let mut address = input.url.trim().to_owned();
    if !url::is_web(&address) {
        return Err(ToolError::InvalidInput(format!(
            "`url` must be a whole http or https address, such as https://example.com/page; \
             got {address:?}"
        )));
    }
    for hops in 0..=MAX_REDIRECTS {
        // A fragment names a place on the page and is not sent.
        let sent = address.split('#').next().unwrap_or(&address).to_owned();
        let header = |name: &str, value: &str| Header {
            name: name.to_owned(),
            value: HeaderValue::Text(value.to_owned()),
        };
        let request = Request {
            method: "GET".to_owned(),
            url: sent,
            headers: vec![
                header("User-Agent", &config.user_agent),
                header(
                    "Accept",
                    "text/html, text/plain, application/json, */*;q=0.5",
                ),
            ],
            body: None,
        };
        let response = http::send(&request).map_err(|error| match error {
            // Said with the address, since after a redirect it is not the one asked for.
            HostError::Denied(reason) => ToolError::Denied(format!("{address}: {reason}")),
            HostError::Failed(reason) => ToolError::Failed(format!("{address}: {reason}")),
        })?;
        let redirect = (300..400)
            .contains(&response.status)
            .then(|| header_of(&response, "location"))
            .flatten();
        let Some(location) = redirect else {
            return render(&address, hops, &response, input, config);
        };
        address = url::resolve(&address, &location).ok_or_else(|| {
            ToolError::Failed(format!(
                "{address} redirects to {location:?}, which is not a web address"
            ))
        })?;
    }
    Err(ToolError::Failed(format!(
        "gave up after {MAX_REDIRECTS} redirects, the last to {address}"
    )))
}

/// The value of the header `name` in `response`, as its own text.
fn header_of(response: &Response, name: &str) -> Option<String> {
    header(response, name).map(str::to_owned)
}

struct WebFetch;

impl Plugin for WebFetch {
    fn activate(config: String) -> Result<PluginInfo, String> {
        let config: Config = corduroy_guest::config(&config)?;
        // A second activation in the same instance keeps the first configuration.
        CONFIG.get_or_init(|| config);
        Ok(PluginInfo::new("fetch", env!("CARGO_PKG_VERSION")).tool(
            ToolSpec::new(
                "web_fetch",
                "Fetch a web page and read it as text: documentation, an issue, a raw file. \
                 HTML comes back as text with its headings, lists and links; text and JSON \
                 as they are. A long page is cut and says the `offset` to read on from. \
                 Scripts are not run, so a page built by them has little to read.",
                r#"{"type":"object","properties":{"url":{"type":"string","description":"The whole address, starting with https:// or http://"},"offset":{"type":"integer","description":"Bytes of the text to pass over, as an earlier call said"},"raw":{"type":"boolean","description":"Return the HTML itself instead of its text"}},"required":["url"]}"#,
            )
            // Fetching a page twice is fetching it once.
            .replay_safe(),
        ))
    }

    fn call_tool(name: String, input: String) -> Result<String, ToolError> {
        match name.as_str() {
            "web_fetch" => fetch(&parse(&input)?),
            _ => Err(ToolError::UnknownTool(name)),
        }
    }
}

corduroy_guest::export!(WebFetch);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_is_what_it_was_sent_as_or_what_it_looks_like() {
        assert_eq!(kind(Some("text/html; charset=utf-8"), b"x"), Kind::Html);
        assert_eq!(kind(Some("application/xhtml+xml"), b"x"), Kind::Html);
        assert_eq!(kind(Some("application/json"), b"{}"), Kind::Text);
        assert_eq!(kind(Some("text/plain"), b"x"), Kind::Text);
        assert_eq!(kind(Some("image/png"), b"\x89PNG"), Kind::Other);
        assert_eq!(kind(Some("application/pdf"), b"%PDF"), Kind::Other);
        assert_eq!(kind(None, b"  <!DOCTYPE HTML><p>x"), Kind::Html);
        assert_eq!(kind(None, b"plain words"), Kind::Text);
        assert_eq!(
            kind(Some("application/octet-stream"), b"# readme"),
            Kind::Text
        );
        assert_eq!(kind(None, &[0xff, 0xfe, 0x00]), Kind::Other);
    }

    #[test]
    fn a_long_text_is_read_a_window_at_a_time_on_whole_characters() {
        let text = "aé".repeat(10);
        let (first, more) = window(&text, 0, 10);
        assert_eq!(first, "aéaéaéa");
        assert_eq!(more, Some(10));
        let (second, more) = window(&text, 10, 100);
        assert_eq!(second, "éaéaéaéaéaéaé");
        assert_eq!(more, None);
        assert_eq!(window(&text, 1000, 10), ("", None));
        assert_eq!(window("", 0, 10), ("", None));
    }
}
