//! Context files in the system prompt, after Goose's `.goosehints` and the `AGENTS.md`
//! convention.
//!
//! A project says how it wants to be worked on in a file at its root; this plugin puts each
//! such file it can read into the system prompt before every model request, so a change to
//! one is seen at the next step. It needs `fs-read` on the files and nothing else, and it
//! skips a file that is missing or that its grant does not reach.
//!
//! Configuration: `files`, the files to look for (`AGENTS.md`, `.goosehints` and
//! `CLAUDE.md` by default; relative paths are in the workspace), and `max_bytes`, how much
//! of them goes in the prompt in all (32 KiB by default).

use corduroy_guest::host::fs_read;
use corduroy_guest::{Plugin, PluginInfo};
use serde::Deserialize;
use std::fmt::Write as _;
use std::sync::OnceLock;

#[derive(Deserialize)]
#[serde(default)]
struct Config {
    files: Vec<String>,
    max_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            files: vec![
                "AGENTS.md".to_owned(),
                ".goosehints".to_owned(),
                "CLAUDE.md".to_owned(),
            ],
            max_bytes: 32 * 1024,
        }
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();

/// The prompt part for `found`, each a file and its contents, within `max_bytes`.
fn render(found: &[(String, String)], max_bytes: usize) -> Option<String> {
    if found.is_empty() {
        return None;
    }
    let mut part = String::from(
        "The project gives these instructions. Follow them unless the user says otherwise.\n",
    );
    for (file, contents) in found {
        let room = max_bytes.saturating_sub(part.len());
        if room == 0 {
            break;
        }
        let mut cut = contents.len().min(room);
        while !contents.is_char_boundary(cut) {
            cut -= 1;
        }
        let shown = contents.get(..cut).unwrap_or_default();
        let more = if cut < contents.len() {
            "\n[cut short]"
        } else {
            ""
        };
        // Writing to a `String` cannot fail.
        let _ = write!(
            part,
            "<hints file=\"{file}\">\n{}{more}\n</hints>\n",
            shown.trim_end()
        );
    }
    Some(part)
}

struct Hints;

impl Plugin for Hints {
    fn activate(config: String) -> Result<PluginInfo, String> {
        let config: Config = corduroy_guest::config(&config)?;
        CONFIG.get_or_init(|| config);
        Ok(PluginInfo::new("hints", env!("CARGO_PKG_VERSION")).with_prompt())
    }

    fn prompt_part() -> Option<String> {
        let config = CONFIG.get()?;
        let found: Vec<(String, String)> = config
            .files
            .iter()
            .filter_map(|file| Some((file.clone(), fs_read::read(file).ok()?)))
            .filter(|(_, contents)| !contents.trim().is_empty())
            .collect();
        render(&found, config.max_bytes)
    }
}

corduroy_guest::export!(Hints);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_file_is_named_and_the_total_is_bounded() {
        let found = vec![
            ("AGENTS.md".to_owned(), "Use tabs.\n".to_owned()),
            (".goosehints".to_owned(), "x".repeat(500)),
        ];
        let part = render(&found, 300).unwrap();
        assert!(part.contains("<hints file=\"AGENTS.md\">\nUse tabs.\n</hints>"));
        assert!(part.contains("[cut short]"));
        assert!(part.len() < 400);
        assert_eq!(render(&[], 300), None);
    }
}
