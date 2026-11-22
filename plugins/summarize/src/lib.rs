//! Summarize files and directories with a model, after Goose's Summarize extension.
//!
//! The model in the loop pays for every byte it reads. This plugin reads files for it,
//! through `fs-read`, and asks a model of its own through `llm` (often a smaller, cheaper
//! one) for what the model needs to know: a summary, or the answer to a question about them.
//!
//! Configuration: `model`, the model to ask, which the host must serve and the `llm` grant
//! must reach; `max_bytes`, the most text sent to it (200 KB by default).
//!
//! Tool: `summarize`.

use std::fmt::Write as _;
use std::sync::OnceLock;

use corduroy_guest::host::fs_read;
use corduroy_guest::host::llm::{self, Completion, Role, Turn};
use corduroy_guest::{Plugin, PluginInfo, ToolError, ToolSpec, parse};
use serde::Deserialize;

/// Files read from one directory, at most.
const MAX_FILES: usize = 200;
/// Directories not opened.
const UNOPENED: [&str; 5] = [".git/", "node_modules/", "target/", ".corduroy/", ".venv/"];

#[derive(Deserialize)]
struct Config {
    model: String,
    #[serde(default = "default_max_bytes")]
    max_bytes: usize,
}

const fn default_max_bytes() -> usize {
    200_000
}

static CONFIG: OnceLock<Config> = OnceLock::new();

#[derive(Deserialize)]
struct Summarize {
    paths: Vec<String>,
    question: Option<String>,
}

/// The files at and below `path`, depth first, at most `MAX_FILES`.
fn files(path: &str, found: &mut Vec<String>) {
    let Ok(entries) = fs_read::list(path) else {
        found.push(path.to_owned());
        return;
    };
    for name in entries {
        if found.len() >= MAX_FILES {
            return;
        }
        let child = format!(
            "{}/{}",
            path.trim_end_matches('/'),
            name.trim_end_matches('/')
        );
        if !name.ends_with('/') {
            found.push(child);
        } else if !UNOPENED.contains(&name.as_str()) {
            files(&child, found);
        }
    }
}

fn summarize(input: &Summarize) -> Result<String, ToolError> {
    let config = CONFIG
        .get()
        .ok_or_else(|| ToolError::Failed("summarize was not configured".to_owned()))?;
    let mut paths = Vec::new();
    for path in &input.paths {
        files(path, &mut paths);
    }
    let mut text = String::new();
    let mut left_out = 0;
    for path in &paths {
        // Files it may not read, or that are not text, are left out.
        let Ok(contents) = fs_read::read(path) else {
            left_out += 1;
            continue;
        };
        if text.len() + contents.len() > config.max_bytes {
            left_out += 1;
            continue;
        }
        // Writing to a `String` cannot fail.
        let _ = write!(text, "=== {path} ===\n{contents}\n");
    }
    if text.is_empty() {
        return Err(ToolError::InvalidInput(
            "none of the paths could be read as text".to_owned(),
        ));
    }
    let ask = input.question.as_deref().map_or_else(
        || {
            "Summarize these files: what each is for, how they fit together, and anything \
            surprising. Be concise and concrete."
                .to_owned()
        },
        |question| format!("Answer from these files: {question}"),
    );
    let request = Completion {
        system: "You read files for another agent and report what it needs to know. Quote \
                 names exactly. Say when the files do not answer the question."
            .to_owned(),
        messages: vec![Turn {
            role: Role::User,
            text: format!("{ask}\n\n{text}"),
        }],
    };
    let answer = llm::complete(&config.model, &request)?;
    let note = if left_out > 0 {
        format!("\n\n[{left_out} file(s) were left out: unreadable, not text, or over the limit]")
    } else {
        String::new()
    };
    Ok(format!("{answer}{note}"))
}

struct Summarizer;

impl Plugin for Summarizer {
    fn activate(config: String) -> Result<PluginInfo, String> {
        let config: Config = corduroy_guest::config(&config)?;
        CONFIG.get_or_init(|| config);
        Ok(PluginInfo::new("summarize", env!("CARGO_PKG_VERSION")).tool(
            ToolSpec::new(
                "summarize",
                "Have files or whole directories read and summarized by another model, or a \
                 question about them answered, without reading them yourself.",
                r#"{"type":"object","properties":{"paths":{"type":"array","items":{"type":"string"}},"question":{"type":"string"}},"required":["paths"]}"#,
            )
            .read_only(),
        ))
    }

    fn call_tool(name: String, input: String) -> Result<String, ToolError> {
        match name.as_str() {
            "summarize" => summarize(&parse(&input)?),
            _ => Err(ToolError::UnknownTool(name)),
        }
    }
}

corduroy_guest::export!(Summarizer);
