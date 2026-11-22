//! Memories the agent saves for later, by category, after Goose's memory extension.
//!
//! A memory is a short note (a preference, a fact about the project, a command that worked)
//! filed under a category. Each category is a Markdown file in the plugin's directory, one
//! entry per paragraph, so a person can read and edit them too. What is saved is put in the
//! system prompt (within a bound), so the next session starts knowing it.
//!
//! Tools: `memory_save`, `memory_recall` and `memory_forget`. Needs `fs-read` and `fs-write`
//! on its directory, `dir` in the configuration (`memory` in the workspace by default).

mod entries;

use std::fmt::Write as _;
use std::sync::OnceLock;

use corduroy_guest::host::{fs_read, fs_write};
use corduroy_guest::{HostError, Plugin, PluginInfo, ToolError, ToolSpec, parse};
use entries::{Entry, category_name};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(default)]
struct Config {
    dir: String,
    /// How much of the saved memories goes in the system prompt.
    prompt_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dir: "memory".to_owned(),
            prompt_bytes: 8 * 1024,
        }
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();

fn config() -> &'static Config {
    CONFIG.get_or_init(Config::default)
}

#[derive(Deserialize)]
struct Save {
    category: String,
    text: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct Recall {
    category: Option<String>,
}

#[derive(Deserialize)]
struct Forget {
    category: String,
    /// Forget only the entries containing this; the whole category without it.
    containing: Option<String>,
}

fn path(category: &str) -> String {
    format!("{}/{category}.md", config().dir.trim_end_matches('/'))
}

/// The entries of `category`, none if it has no file yet.
fn load(category: &str) -> Result<Vec<Entry>, ToolError> {
    match fs_read::read(&path(category)) {
        Ok(text) => Ok(entries::parse(&text)),
        Err(HostError::Failed(_)) => Ok(Vec::new()),
        Err(denied) => Err(denied.into()),
    }
}

fn store(category: &str, entries: &[Entry]) -> Result<(), ToolError> {
    if entries.is_empty() {
        return match fs_write::remove(&path(category)) {
            Ok(()) | Err(HostError::Failed(_)) => Ok(()),
            Err(denied) => Err(denied.into()),
        };
    }
    fs_write::write(&path(category), &entries::render(entries))?;
    Ok(())
}

/// Every category there is, sorted.
fn categories() -> Vec<String> {
    fs_read::list(&config().dir)
        .unwrap_or_default()
        .iter()
        .filter_map(|name| name.strip_suffix(".md"))
        .filter(|name| category_name(name).is_ok())
        .map(str::to_owned)
        .collect()
}

fn save(input: &Save) -> Result<String, ToolError> {
    let category = category_name(&input.category).map_err(ToolError::InvalidInput)?;
    let text = input.text.trim();
    if text.is_empty() {
        return Err(ToolError::InvalidInput("text is empty".to_owned()));
    }
    let mut saved = load(category)?;
    if saved.iter().any(|entry| entry.text == text) {
        return Ok(format!("Already remembered under `{category}`."));
    }
    saved.push(Entry {
        text: text.to_owned(),
        tags: input.tags.clone(),
    });
    store(category, &saved)?;
    Ok(format!(
        "Remembered under `{category}` ({} entries).",
        saved.len()
    ))
}

fn recall(input: &Recall) -> Result<String, ToolError> {
    let wanted = match &input.category {
        Some(category) => vec![
            category_name(category)
                .map_err(ToolError::InvalidInput)?
                .to_owned(),
        ],
        None => categories(),
    };
    let mut out = String::new();
    for category in wanted {
        let saved = load(&category)?;
        if !saved.is_empty() {
            // Writing to a `String` cannot fail.
            let _ = write!(out, "## {category}\n{}\n", entries::render(&saved));
        }
    }
    Ok(if out.is_empty() {
        "Nothing is remembered yet.".to_owned()
    } else {
        out
    })
}

fn forget(input: &Forget) -> Result<String, ToolError> {
    let category = category_name(&input.category).map_err(ToolError::InvalidInput)?;
    let saved = load(category)?;
    let kept: Vec<Entry> = match &input.containing {
        Some(needle) => saved
            .iter()
            .filter(|entry| !entry.text.contains(needle.as_str()))
            .cloned()
            .collect(),
        None => Vec::new(),
    };
    let forgotten = saved.len() - kept.len();
    store(category, &kept)?;
    Ok(format!(
        "Forgot {forgotten} entr{} from `{category}`.",
        if forgotten == 1 { "y" } else { "ies" }
    ))
}

struct Memory;

impl Plugin for Memory {
    fn activate(config: String) -> Result<PluginInfo, String> {
        let config: Config = corduroy_guest::config(&config)?;
        CONFIG.get_or_init(|| config);
        let category = r#""category":{"type":"string","description":"A short lowercase name such as preferences, project or commands"}"#;
        Ok(PluginInfo::new("memory", env!("CARGO_PKG_VERSION"))
            .tool(
                ToolSpec::new(
                    "memory_save",
                    "Remember something for later sessions: a preference the user stated, a \
                     fact about the project, a command that worked. Filed under a category.",
                    &format!(
                        r#"{{"type":"object","properties":{{{category},"text":{{"type":"string"}},"tags":{{"type":"array","items":{{"type":"string"}}}}}},"required":["category","text"]}}"#
                    ),
                )
                // Saving the same text again changes nothing.
                .replay_safe(),
            )
            .tool(
                ToolSpec::new(
                    "memory_recall",
                    "Read what was remembered, in one category or in all of them.",
                    &format!(r#"{{"type":"object","properties":{{{category}}}}}"#),
                )
                .read_only()
                .replay_safe(),
            )
            .tool(ToolSpec::new(
                "memory_forget",
                "Forget the entries of a category that contain some text, or the whole \
                 category.",
                &format!(
                    r#"{{"type":"object","properties":{{{category},"containing":{{"type":"string"}}}},"required":["category"]}}"#
                ),
            ))
            .with_prompt())
    }

    fn call_tool(name: String, input: String) -> Result<String, ToolError> {
        match name.as_str() {
            "memory_save" => save(&parse(&input)?),
            "memory_recall" => recall(&parse(&input)?),
            "memory_forget" => forget(&parse(&input)?),
            _ => Err(ToolError::UnknownTool(name)),
        }
    }

    fn prompt_part() -> Option<String> {
        let all = recall(&Recall { category: None }).ok()?;
        if categories().is_empty() {
            return None;
        }
        let limit = config().prompt_bytes;
        let mut cut = all.len().min(limit);
        while !all.is_char_boundary(cut) {
            cut -= 1;
        }
        let more = if cut < all.len() {
            "\n[more: use memory_recall]"
        } else {
            ""
        };
        Some(format!(
            "<memories>\n{}{more}\n</memories>\nThese are things you saved with memory_save in earlier sessions.",
            all.get(..cut).unwrap_or_default().trim_end()
        ))
    }
}

corduroy_guest::export!(Memory);
