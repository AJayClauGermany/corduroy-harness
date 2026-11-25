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
    /// The plugin path is each directory in `CORDUROY_PLUGIN_PATH`, then the `plugins`
    /// directory beside the running program (where a downloaded build keeps its add-ons),
    /// then the plugins of the source tree this was built from, and only then the agent
    /// file's directory. What ships with the command comes before what a workspace holds, so
    /// a repository cannot stand its own `approval.wasm` in for the shipped one; what the
    /// person names in their own environment comes before both.
    #[must_use]
    pub fn new(workspace: PathBuf, file: Option<PathBuf>) -> Self {
        let mut plugin_path: Vec<PathBuf> = Vec::new();
        if let Some(extra) = std::env::var_os("CORDUROY_PLUGIN_PATH") {
            plugin_path.extend(std::env::split_paths(&extra));
        }
        plugin_path.extend(beside_program());
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/target");
        plugin_path.push(source.join("wasm32-unknown-unknown/release"));
        plugin_path.push(source.join("guests"));
        plugin_path.extend(file.as_deref().and_then(Path::parent).map(Path::to_owned));
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

/// The `plugins` directory beside the running program, symbolic links resolved, so that a
/// build unpacked anywhere (or linked onto `PATH`) finds the add-ons shipped with it.
fn beside_program() -> Option<PathBuf> {
    let program = std::env::current_exe().ok()?;
    let program = std::fs::canonicalize(&program).unwrap_or(program);
    Some(program.parent()?.join("plugins"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugins_beside_the_program_come_before_the_source_tree() {
        let places = Places::new(PathBuf::from("/work"), None);
        let beside = beside_program().unwrap();
        let at = |dir: &Path| places.plugin_path.iter().position(|entry| entry == dir);
        let source = places
            .plugin_path
            .iter()
            .position(|dir| dir.ends_with("wasm32-unknown-unknown/release"))
            .unwrap();
        assert!(at(&beside).unwrap() < source);
    }

    #[test]
    fn what_ships_with_the_command_comes_before_what_is_beside_the_agent_file() {
        let file = PathBuf::from("/work/corduroy.toml");
        let places = Places::new(PathBuf::from("/work"), Some(file));
        let at = |dir: &Path| places.plugin_path.iter().position(|entry| entry == dir);
        let beside_file = at(Path::new("/work")).unwrap();
        // A workspace's own `approval.wasm` never stands in for the shipped one.
        assert!(at(&beside_program().unwrap()).unwrap() < beside_file);
        assert_eq!(beside_file, places.plugin_path.len() - 1);
    }
}
