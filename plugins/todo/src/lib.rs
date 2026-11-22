//! A todo list the agent keeps for itself, after Goose's todo extension.
//!
//! On a long task the model writes down its plan and ticks it off. The list is kept in the
//! plugin's `state` store, so it survives a restart of the plugin and of the harness, and it
//! is shown in the system prompt while it is not empty, so the plan stays in view as the
//! conversation grows.
//!
//! Tools: `todo_read`, and `todo_write`, which replaces the whole list. Needs `state`.

use corduroy_guest::host::state;
use corduroy_guest::{Plugin, PluginInfo, ToolError, ToolSpec, parse};
use serde::Deserialize;

/// The state key the list is kept under.
const KEY: &str = "list";
/// The longest list kept, in bytes (the `state` store holds 64 KiB a value).
const MAX_BYTES: usize = 32 * 1024;

#[derive(Deserialize)]
struct Write {
    content: String,
}

fn current() -> Result<String, ToolError> {
    Ok(state::get(KEY)?.unwrap_or_default())
}

struct Todo;

impl Plugin for Todo {
    fn activate(_config: String) -> Result<PluginInfo, String> {
        Ok(PluginInfo::new("todo", env!("CARGO_PKG_VERSION"))
            .tool(
                ToolSpec::new(
                    "todo_read",
                    "Read your todo list.",
                    r#"{"type":"object","properties":{}}"#,
                )
                .read_only()
                .replay_safe(),
            )
            .tool(
                ToolSpec::new(
                    "todo_write",
                    "Replace your todo list with `content`, for example a Markdown checklist. \
                     Use it to plan a task of several steps and to tick steps off as you go; \
                     write an empty list when the task is done.",
                    r#"{"type":"object","properties":{"content":{"type":"string"}},"required":["content"]}"#,
                )
                // Writing the same list twice is writing it once.
                .replay_safe(),
            )
            .with_prompt())
    }

    fn call_tool(name: String, input: String) -> Result<String, ToolError> {
        match name.as_str() {
            "todo_read" => {
                let list = current()?;
                Ok(if list.trim().is_empty() {
                    "(the list is empty)".to_owned()
                } else {
                    list
                })
            }
            "todo_write" => {
                let Write { content } = parse(&input)?;
                if content.len() > MAX_BYTES {
                    return Err(ToolError::InvalidInput(format!(
                        "the list is {} bytes; keep it under {MAX_BYTES}",
                        content.len()
                    )));
                }
                if content.trim().is_empty() {
                    state::remove(KEY)?;
                    return Ok("Cleared the todo list.".to_owned());
                }
                state::set(KEY, &content)?;
                Ok(format!(
                    "Saved the todo list ({} lines).",
                    content.lines().count()
                ))
            }
            _ => Err(ToolError::UnknownTool(name)),
        }
    }

    fn prompt_part() -> Option<String> {
        let list = current().ok()?;
        (!list.trim().is_empty()).then(|| {
            format!(
                "<todo>\n{}\n</todo>\nThis is your todo list. Keep it current with todo_write.",
                list.trim_end()
            )
        })
    }
}

corduroy_guest::export!(Todo);
