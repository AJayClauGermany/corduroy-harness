//! Extension kinds: the native plugins an agent file can ask for.
//!
//! An *extension* is a native plugin: Rust compiled into the command, trusted with the host's
//! authority, and loaded into the same plugin tree as everything else. It is how an agent
//! gets what the core deliberately does not have: a kernel that runs in a container, a step
//! of the loop, a decision point of its own, a service add-ons call. Being compiled in, the
//! extensions an agent file can ask for are the *kinds* its command was built with, and a
//! new kind means rebuilding the command; an add-on (a WebAssembly plugin) needs no rebuild:
//!
//! ```toml
//! [extensions.scratch]
//! kind = "shell-kernel"            # every other key is the extension's own configuration
//! dir = "{workspace}/scratch"
//! ```
//!
//! A kernel entry is built the same way: `[kernels.<name>]` with `kind = "shell"` asks for the
//! kind `shell-kernel` ([`kernel_kind`]), so a command built for another system can bring a
//! kernel of its own under a kind of its own, or under `shell-kernel` itself.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use corduroy_harness::core::Plugin;
use corduroy_harness::shell::{ShellKernel, shell_kernel_of};
use serde_json::Value;

use crate::places::Places;

/// The extension kind a `[kernels.<name>]` entry of `kind` is built from: `<kind>-kernel`.
#[must_use]
pub fn kernel_kind(kind: &str) -> String {
    format!("{kind}-kernel")
}

/// One `[extensions.<name>]` (or `[kernels.<name>]`) entry, as the function that builds its
/// kind sees it.
#[derive(Debug, Clone, Copy)]
pub struct Asked<'a> {
    /// The entry's name. The plugin built should go by it, or by a name made from it.
    pub name: &'a str,
    /// The entry's keys other than `kind`, as a JSON object, with `{workspace}`, `{data}`
    /// and `{home}` filled in.
    pub config: &'a Value,
    /// Where the agent works and keeps its things.
    pub places: &'a Places,
}

impl Asked<'_> {
    /// The string under `key` of the configuration, if there is one.
    #[must_use]
    pub fn text(&self, key: &str) -> Option<&str> {
        self.config.get(key).and_then(Value::as_str)
    }

    /// Refuse a configuration with a key other than `keys`, so that a misspelt key is an
    /// error rather than a setting silently ignored.
    ///
    /// # Errors
    ///
    /// Names the first key that is not one of `keys`, and the keys there are.
    pub fn only(&self, keys: &[&str]) -> Result<(), String> {
        let unknown = self
            .config
            .as_object()
            .and_then(|config| config.keys().find(|key| !keys.contains(&key.as_str())));
        match unknown {
            Some(key) if keys.is_empty() => Err(format!("`{key}` is not a setting of this kind")),
            Some(key) => Err(format!(
                "`{key}` is not a setting of this kind (it has: {})",
                keys.join(", ")
            )),
            None => Ok(()),
        }
    }
}

/// The plugin of an extension, or what is wrong with its entry.
pub type Built = Result<Box<dyn Plugin>, String>;

/// Builds the plugin of one extension kind.
type Build = dyn Fn(&Asked<'_>) -> Built + Send + Sync;

/// The extension kinds a command was built with, by name.
#[derive(Clone, Default)]
pub struct Extensions {
    kinds: BTreeMap<String, Arc<Build>>,
}

impl fmt::Debug for Extensions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Extensions").field(&self.kinds()).finish()
    }
}

impl Extensions {
    /// No kinds at all.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// The kinds the standard extensions bring:
    ///
    /// | Kind | Configuration | What it adds |
    /// | --- | --- | --- |
    /// | `shell-kernel` | `dir` (default `{workspace}`), `program` (default `sh`; any POSIX shell, such as `bash`, Git for Windows' included) | a persistent shell as the kernel named like the entry; what `[kernels.<name>]` with `kind = "shell"` builds |
    /// | `http-client` | none | the client the `http` capability sends through, for `http` and `https` |
    #[must_use]
    pub fn builtin() -> Self {
        Self::none()
            .kind(kernel_kind("shell"), |asked| {
                asked.only(&["dir", "program"])?;
                let dir = asked
                    .text("dir")
                    .map_or_else(|| asked.places.workspace.clone(), PathBuf::from);
                let shell = asked
                    .text("program")
                    .map_or_else(ShellKernel::new, ShellKernel::program);
                Ok(Box::new(shell_kernel_of(
                    asked.name.to_owned(),
                    shell.in_dir(dir),
                )))
            })
            .kind("http-client", |asked| {
                asked.only(&[])?;
                Ok(Box::new(corduroy_http::http_client()))
            })
    }

    /// Add the kind `name`, built by `build`. A kind of that name already there is replaced.
    #[must_use]
    pub fn kind(
        mut self,
        name: impl Into<String>,
        build: impl Fn(&Asked<'_>) -> Built + Send + Sync + 'static,
    ) -> Self {
        self.kinds.insert(name.into(), Arc::new(build));
        self
    }

    /// Add every kind of `other`, replacing kinds of the same name.
    #[must_use]
    pub fn merged(mut self, other: &Self) -> Self {
        self.kinds.extend(
            other
                .kinds
                .iter()
                .map(|(name, build)| (name.clone(), Arc::clone(build))),
        );
        self
    }

    /// Without the kind `name`, if there was one.
    #[must_use]
    pub fn without(mut self, name: &str) -> Self {
        self.kinds.remove(name);
        self
    }

    /// Whether there is a kind `name`.
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.kinds.contains_key(name)
    }

    /// The names of the kinds, sorted.
    #[must_use]
    pub fn kinds(&self) -> Vec<&str> {
        self.kinds.keys().map(String::as_str).collect()
    }

    /// The kinds a `[kernels.<name>]` entry can have: those of the `<kind>-kernel` kinds.
    #[must_use]
    pub fn kernel_kinds(&self) -> Vec<&str> {
        self.kinds
            .keys()
            .filter_map(|kind| kind.strip_suffix("-kernel"))
            .collect()
    }

    /// Build the plugin `asked` describes as an extension of `kind`. `None` if there is no
    /// such kind.
    #[must_use]
    pub fn build(&self, kind: &str, asked: &Asked<'_>) -> Option<Built> {
        self.kinds.get(kind).map(|build| build(asked))
    }
}
