//! Today's date, where the model can see it.
//!
//! A model does not know what day it is: it knows roughly when its training ended, and
//! takes that for now. So it calls last year's release the newest, and works out "three
//! days ago" from the wrong day. This plugin puts today's date in the system prompt, read
//! from the host's clock through the `clock` capability, and offers the time as a tool for
//! the moments a date is not enough.
//!
//! The date changes once a day, so the system prompt stays the same from one request to
//! the next and a provider's cache of it holds.
//!
//! Configuration: `utc_offset`, how far your clock is from UTC, as `+01:00` or `-08:00`.
//! An add-on cannot see the machine's time zone, so without it dates and times are UTC, and
//! say so.
//!
//! Tool: `now`. Needs `clock` (`wall`).

mod date;

use std::sync::OnceLock;

use corduroy_guest::host::clock;
use corduroy_guest::{Plugin, PluginInfo, ToolError, ToolSpec};
use serde::Deserialize;

use date::{Moment, Offset};

/// Plugin configuration, from the host.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Config {
    /// How far the person's clock is from UTC, as `+01:00`.
    utc_offset: Option<String>,
}

static OFFSET: OnceLock<Offset> = OnceLock::new();

/// This moment on the person's clock.
fn now() -> Result<Moment, ToolError> {
    let time = clock::now()?;
    Ok(Moment::at(
        time.seconds,
        OFFSET.get().copied().unwrap_or_default(),
    ))
}

struct Today;

impl Plugin for Today {
    fn activate(config: String) -> Result<PluginInfo, String> {
        let config: Config = corduroy_guest::config(&config)?;
        let offset = config
            .utc_offset
            .as_deref()
            .map(Offset::parse)
            .transpose()
            .map_err(|error| format!("bad config: `utc_offset`: {error}"))?
            .unwrap_or_default();
        // A second activation in the same instance keeps the first configuration.
        OFFSET.get_or_init(|| offset);
        Ok(PluginInfo::new("today", env!("CARGO_PKG_VERSION"))
            .tool(
                ToolSpec::new(
                    "now",
                    "The date and the time right now. Today's date is already in your \
                     instructions; use this when you need the time as well.",
                    r#"{"type":"object","properties":{}}"#,
                )
                .read_only()
                .replay_safe(),
            )
            .with_prompt())
    }

    fn call_tool(name: String, _input: String) -> Result<String, ToolError> {
        match name.as_str() {
            "now" => Ok(now()?.date_and_time()),
            _ => Err(ToolError::UnknownTool(name)),
        }
    }

    fn prompt_part() -> Option<String> {
        let today = now().ok()?.date();
        Some(format!(
            "Today is {today}. Your own sense of the date comes from your training and is \
             behind; go by this one."
        ))
    }
}

corduroy_guest::export!(Today);
