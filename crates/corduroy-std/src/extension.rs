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
//! The agent file's other native entries are built the same way. `[kernels.<name>]` with
//! `kind = "shell"` asks for the kind `shell-kernel` ([`kernel_kind`]), `[mcp.<name>]` for the
//! kind [`MCP`] and `[secrets.<name>]` for the kind [`ENV_SECRET`], each with the entry's keys
//! as its configuration. So a command built for another system can bring a kernel, an MCP
//! client or a source of secrets of its own, under a kind of its own or under the standard
//! kind's name.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use corduroy_harness::core::{Plugin, SECRETS, Secret, Secrets, Spawn, plugin_fn};
use corduroy_mcp::{Transport, mcp_server};
use corduroy_shell::{ShellKernel, shell_kernel_of};
use serde_json::Value;

use crate::places::Places;

/// The extension kind a `[mcp.<name>]` entry is built from.
pub const MCP: &str = "mcp";

/// The extension kind a `[secrets.<name>]` entry is built from.
pub const ENV_SECRET: &str = "env-secret";

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

    /// The strings under `key`: a list of strings, or none if there is no `key`.
    ///
    /// # Errors
    ///
    /// If `key` holds something other than a list of strings.
    pub fn texts(&self, key: &str) -> Result<Vec<String>, String> {
        let Some(value) = self.config.get(key) else {
            return Ok(Vec::new());
        };
        value
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .map(|item| item.as_str().map(str::to_owned))
                    .collect()
            })
            .ok_or_else(|| format!("`{key}` must be a list of strings"))
    }

    /// The table of strings under `key`, or an empty one if there is no `key`.
    ///
    /// # Errors
    ///
    /// If `key` holds something other than a table of strings.
    pub fn table(&self, key: &str) -> Result<BTreeMap<String, String>, String> {
        let Some(value) = self.config.get(key) else {
            return Ok(BTreeMap::new());
        };
        value
            .as_object()
            .and_then(|fields| {
                fields
                    .iter()
                    .map(|(name, item)| item.as_str().map(|item| (name.clone(), item.to_owned())))
                    .collect()
            })
            .ok_or_else(|| format!("`{key}` must be a table of strings"))
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

/// The kernel `shell` asks for, in the directory it names (`{workspace}` by default).
fn shell_kernel_in(asked: &Asked<'_>, shell: ShellKernel) -> Built {
    asked.only(&["dir", "program"])?;
    let dir = asked
        .text("dir")
        .map_or_else(|| asked.places.workspace.clone(), PathBuf::from);
    Ok(Box::new(shell_kernel_of(
        asked.name.to_owned(),
        shell.in_dir(dir),
    )))
}

/// The MCP server `[mcp.<name>]` describes: a `command` started through a kernel (`shell` by
/// default), or a `url`.
fn mcp_server_in(asked: &Asked<'_>) -> Built {
    asked.only(&[
        "command",
        "args",
        "env",
        "kernel",
        "url",
        "bearer_token_env",
        "timeout_seconds",
    ])?;
    let transport = match (asked.text("command"), asked.text("url")) {
        (Some(command), None) => Transport::Stdio {
            kernel: asked.text("kernel").unwrap_or("shell").to_owned(),
            spawn: Spawn {
                program: command.to_owned(),
                args: asked.texts("args")?,
                env: asked.table("env")?.into_iter().collect(),
            },
        },
        (None, Some(url)) => {
            let bearer_token = asked
                .text("bearer_token_env")
                .map(|variable| {
                    std::env::var(variable).map_err(|_| format!("${variable} is not set"))
                })
                .transpose()?;
            Transport::Http {
                url: url.to_owned(),
                bearer_token,
            }
        }
        _ => return Err("needs a `command` or a `url`, not both".to_owned()),
    };
    let server = mcp_server(asked.name, transport);
    Ok(Box::new(match asked.config.get("timeout_seconds") {
        None => server,
        Some(seconds) => server.with_timeout(Duration::from_secs(
            seconds
                .as_u64()
                .ok_or("`timeout_seconds` must be a whole number")?,
        )),
    }))
}

/// The secret `[secrets.<name>]` describes, read from the environment variable `env` each
/// time a plugin's request needs it.
fn env_secret_in(asked: &Asked<'_>) -> Built {
    asked.only(&["env"])?;
    let variable = asked.text("env").ok_or("`env` is required")?.to_owned();
    let name = asked.name.to_owned();
    Ok(Box::new(plugin_fn(
        format!("secret-{name}"),
        &[SECRETS],
        move |ctx| {
            let variable = variable.clone();
            let secret: Arc<dyn Secret> = Arc::new(move || {
                std::env::var(&variable).map_err(|_| format!("${variable} is not set"))
            });
            ctx.service::<Secrets>(SECRETS)?
                .register(ctx, name.clone(), secret)?;
            Ok(())
        },
    )))
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
    /// | `powershell-kernel` | `dir` (default `{workspace}`), `program` (default `powershell` on Windows, `pwsh` elsewhere) | a persistent PowerShell as the kernel named like the entry; what `[kernels.<name>]` with `kind = "powershell"` builds |
    /// | `http-client` | none | the client the `http` capability sends through, for `http` and `https` |
    /// | `mcp` | `command` (with `args`, `env` and `kernel`, `shell` by default) or `url` (with `bearer_token_env`), and `timeout_seconds` | the tools of one MCP server, as `<name>__<tool>`; what `[mcp.<name>]` builds |
    /// | `env-secret` | `env` | the secret named like the entry, read from that environment variable when it is used; what `[secrets.<name>]` builds |
    #[must_use]
    pub fn builtin() -> Self {
        Self::none()
            .kind(kernel_kind("shell"), |asked| {
                let shell = asked
                    .text("program")
                    .map_or_else(ShellKernel::new, ShellKernel::program);
                shell_kernel_in(asked, shell)
            })
            .kind(kernel_kind("powershell"), |asked| {
                let shell = asked
                    .text("program")
                    .map_or_else(ShellKernel::powershell, ShellKernel::powershell_program);
                shell_kernel_in(asked, shell)
            })
            .kind("http-client", |asked| {
                asked.only(&[])?;
                Ok(Box::new(corduroy_http::http_client()))
            })
            .kind(MCP, mcp_server_in)
            .kind(ENV_SECRET, env_secret_in)
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
