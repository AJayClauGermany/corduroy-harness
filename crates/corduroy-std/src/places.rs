//! Where an agent works and keeps its things.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// The agent's own directory in the workspace: its log, compiled-code cache and skills.
pub const DATA_DIR: &str = ".corduroy";

/// Where an agent works and keeps its things.
#[derive(Debug, Clone)]
pub struct Places {
    /// The directory the agent works in.
    pub workspace: PathBuf,
    /// The agent's own directory, `{workspace}/.corduroy`.
    pub data: PathBuf,
    /// The agent file, if it came from one.
    pub file: Option<PathBuf>,
    /// Directories searched for a plugin given by a relative path, in order.
    pub plugin_path: Vec<PathBuf>,
}

impl Places {
    /// The places for an agent working in `workspace`, described by `file`.
    ///
    /// The plugin path is the agent file's directory, then each directory in
    /// `CORDUROY_PLUGIN_PATH`, then the plugins of the source tree this was built from.
    #[must_use]
    pub fn new(workspace: PathBuf, file: Option<PathBuf>) -> Self {
        let mut plugin_path: Vec<PathBuf> = file
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_owned)
            .into_iter()
            .collect();
        if let Some(extra) = std::env::var_os("CORDUROY_PLUGIN_PATH") {
            plugin_path.extend(std::env::split_paths(&extra));
        }
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/target");
        plugin_path.push(source.join("wasm32-unknown-unknown/release"));
        plugin_path.push(source.join("guests"));
        Self {
            data: workspace.join(DATA_DIR),
            workspace,
            file,
            plugin_path,
        }
    }

    /// `text` with `{workspace}`, `{data}` and `{home}` filled in. The home directory is
    /// `HOME`, or `USERPROFILE` on a system that has none.
    #[must_use]
    pub fn expand(&self, text: &str) -> String {
        let home_dir = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_default();
        text.replace("{workspace}", &self.workspace.to_string_lossy())
            .replace("{data}", &self.data.to_string_lossy())
            .replace("{home}", &home_dir)
    }

    /// `value` with every string in it [expanded](Self::expand).
    #[must_use]
    pub fn expand_json(&self, value: Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.expand(&text)),
            Value::Array(items) => Value::Array(
                items
                    .into_iter()
                    .map(|item| self.expand_json(item))
                    .collect(),
            ),
            Value::Object(fields) => Value::Object(
                fields
                    .into_iter()
                    .map(|(key, value)| (key, self.expand_json(value)))
                    .collect(),
            ),
            other => other,
        }
    }

    /// The file a plugin entry names: `path` expanded, as it is if absolute, and otherwise
    /// the first match on the plugin path.
    ///
    /// # Errors
    ///
    /// Says where it looked, if a relative `path` is on none of the plugin path.
    pub fn find(&self, path: &str) -> Result<PathBuf, String> {
        let path = PathBuf::from(self.expand(path));
        if path.is_absolute() {
            return Ok(path);
        }
        self.plugin_path
            .iter()
            .map(|dir| dir.join(&path))
            .find(|candidate| candidate.is_file())
            .ok_or_else(|| {
                let searched: Vec<String> = self
                    .plugin_path
                    .iter()
                    .map(|dir| dir.display().to_string())
                    .collect();
                format!(
                    "plugin `{}` not found in {}. Build the plugins (`cargo plugins`, \
                     `scripts/guests.sh`) or set CORDUROY_PLUGIN_PATH",
                    path.display(),
                    searched.join(", ")
                )
            })
    }
}
