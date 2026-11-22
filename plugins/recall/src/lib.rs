//! Search earlier conversations, after Goose's Chat Recall.
//!
//! Every conversation is on the event stream, so earlier sessions can be searched without a
//! store of their own: this plugin reads `message` events through the `log-read` capability,
//! scoped by session and topic, and finds the ones whose text holds every word of a query.
//! Grant it `log-read` on `*:message` and `*:session/start` for every session, or narrow the
//! session part to keep some out of reach.
//!
//! Tools: `recall_sessions` and `recall_search`.

mod text;

use corduroy_guest::host::log_read;
use corduroy_guest::{Plugin, PluginInfo, ToolError, ToolSpec, parse};
use serde::Deserialize;

/// Events read per page (the host's limit).
const PAGE: u32 = 1000;
/// Message events searched at most, newest pages last.
const SCANNED: usize = 50_000;

#[derive(Deserialize)]
struct Search {
    query: String,
    session: Option<String>,
    #[serde(default = "ten")]
    limit: usize,
}

const fn ten() -> usize {
    10
}

/// Every event of `topic` the grant reaches, in `session` if given, oldest first.
fn read_all(session: Option<&str>, topic: &str) -> Result<Vec<corduroy_guest::Event>, ToolError> {
    let mut events = Vec::new();
    let mut after = 0;
    loop {
        let page = log_read::read(session, topic, after, PAGE)?;
        let Some(last) = page.last() else {
            break;
        };
        after = last.id;
        let full = page.len() == PAGE as usize;
        events.extend(page);
        if !full || events.len() >= SCANNED {
            break;
        }
    }
    Ok(events)
}

fn sessions() -> Result<String, ToolError> {
    let started = read_all(None, "session/start")?;
    if started.is_empty() {
        return Ok("No sessions are within reach.".to_owned());
    }
    let mut lines = Vec::new();
    for start in started {
        let Some(session) = start.session else {
            continue;
        };
        let first = log_read::read(Some(&session), "message", start.id, 1)?
            .into_iter()
            .next()
            .map(|event| text::of(&event.payload))
            .unwrap_or_default();
        lines.push(format!("{session}: {}", text::snippet(&first, "", 100)));
    }
    Ok(lines.join("\n"))
}

fn search(input: &Search) -> Result<String, ToolError> {
    let words: Vec<String> = input
        .query
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    let Some(first_word) = words.first() else {
        return Err(ToolError::InvalidInput("the query is empty".to_owned()));
    };
    let messages = read_all(input.session.as_deref(), "message")?;
    let found: Vec<String> = messages
        .iter()
        .rev()
        .filter_map(|event| {
            let said = text::of(&event.payload);
            let lower = said.to_lowercase();
            if !words.iter().all(|word| lower.contains(word.as_str())) {
                return None;
            }
            let session = event.session.as_deref().unwrap_or("-");
            Some(format!(
                "[{session} #{}] {}: {}",
                event.id,
                event.source,
                text::snippet(&said, first_word, 200)
            ))
        })
        .take(input.limit.clamp(1, 50))
        .collect();
    Ok(if found.is_empty() {
        format!("Nothing earlier mentions \"{}\".", input.query)
    } else {
        found.join("\n")
    })
}

struct Recall;

impl Plugin for Recall {
    fn activate(_config: String) -> Result<PluginInfo, String> {
        Ok(PluginInfo::new("recall", env!("CARGO_PKG_VERSION"))
            .tool(
                ToolSpec::new(
                    "recall_sessions",
                    "List earlier conversations, each with how it began.",
                    r#"{"type":"object","properties":{}}"#,
                )
                .read_only()
                .replay_safe(),
            )
            .tool(
                ToolSpec::new(
                    "recall_search",
                    "Search earlier conversations for messages holding every word of a query, \
                     newest first.",
                    r#"{"type":"object","properties":{"query":{"type":"string"},"session":{"type":"string","description":"Search only this session"},"limit":{"type":"integer","description":"Most results (default 10)"}},"required":["query"]}"#,
                )
                .read_only()
                .replay_safe(),
            ))
    }

    fn call_tool(name: String, input: String) -> Result<String, ToolError> {
        match name.as_str() {
            "recall_sessions" => sessions(),
            "recall_search" => search(&parse(&input)?),
            _ => Err(ToolError::UnknownTool(name)),
        }
    }
}

corduroy_guest::export!(Recall);
