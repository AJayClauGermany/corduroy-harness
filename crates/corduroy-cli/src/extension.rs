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
//! A command of your own adds kinds and is otherwise the same command:
//!
//! ```no_run
//! use corduroy_cli::command::main_with;
//! use corduroy_cli::extension::Extensions;
//! use corduroy_harness::core::plugin_fn;
//!
//! fn main() -> std::process::ExitCode {
//!     let extensions = Extensions::builtin().kind("greeter", |asked| {
//!         let greeting = asked.text("greeting").unwrap_or("hello").to_owned();
//!         Ok(Box::new(plugin_fn(asked.name, &[], move |ctx| {
//!             ctx.emit("greeter/ready", format!("{greeting:?}"))?;
//!             Ok(())
//!         })))
//!     });
//!     main_with(&extensions)
//! }
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use corduroy_harness::core::Plugin;
use corduroy_harness::shell_kernel;
use serde_json::Value;

use crate::agent::Places;

/// One `[extensions.<name>]` entry, as the function that builds its kind sees it.
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

    /// The kinds the `corduroy` command ships with:
    ///
    /// | Kind | Configuration | What it adds |
    /// | --- | --- | --- |
    /// | `shell-kernel` | `dir` (default `{workspace}`) | a persistent shell as the kernel named like the entry, as `[kernels.<name>]` with `kind = "shell"` does |
    #[must_use]
    pub fn builtin() -> Self {
        Self::none().kind("shell-kernel", |asked| {
            let dir = asked
                .text("dir")
                .map_or_else(|| asked.places.workspace.clone(), PathBuf::from);
            Ok(Box::new(shell_kernel(asked.name.to_owned(), dir)))
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

    /// The names of the kinds, sorted.
    #[must_use]
    pub fn kinds(&self) -> Vec<&str> {
        self.kinds.keys().map(String::as_str).collect()
    }

    /// Build the plugin `asked` describes as an extension of `kind`. `None` if there is no
    /// such kind.
    pub(crate) fn build(&self, kind: &str, asked: &Asked<'_>) -> Option<Built> {
        self.kinds.get(kind).map(|build| build(asked))
    }
}
