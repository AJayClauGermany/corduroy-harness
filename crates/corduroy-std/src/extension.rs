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
//! kind [`MCP`] and `[secrets.<name>]` for the kind [`KEYCHAIN_SECRET`], or [`ENV_SECRET`]
//! where the entry names a variable, or `<kind>-secret` where it gives a `kind`
//! ([`secret_kind`]: [`CHATGPT_SECRET`]), each
//! with the entry's keys as its configuration. So a command built for another system can
//! bring a kernel, an MCP client or a source of secrets of its own, under a kind of its own
//! or under the standard kind's name.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use corduroy_chatgpt::{ChatGpt, Store, Vault, chatgpt_secret};
use corduroy_harness::core::{Plugin, SECRETS, Secret, Secrets, Spawn, plugin_fn};
use corduroy_keychain::{Keychain, Os, keychain_secret};
use corduroy_mcp::{Transport, mcp_server};
use corduroy_shell::{ShellKernel, shell_kernel_of};
use serde_json::Value;

use crate::places::Places;

/// The extension kind a `[mcp.<name>]` entry is built from.
pub const MCP: &str = "mcp";

/// The extension kind of a secret read from the environment: what a `[secrets.<name>]` entry
/// that names a variable (`env`) is built from.
pub const ENV_SECRET: &str = "env-secret";

/// The extension kind of a secret that is a `ChatGPT` sign-in: what a `[secrets.<name>]`
/// entry with `kind = "chatgpt"` is built from.
pub const CHATGPT_SECRET: &str = "chatgpt-secret";

/// The extension kind of a secret kept in the system's keychain: what a `[secrets.<name>]`
/// entry is built from when it says `kind = "keychain"`, or says nothing at all.
pub const KEYCHAIN_SECRET: &str = "keychain-secret";

/// The extension kind that serves the agent's page in a browser.
pub const PAGE: &str = "page";

/// The extension kind a `[secrets.<name>]` entry of `kind` is built from: `<kind>-secret`.
#[must_use]
pub fn secret_kind(kind: &str) -> String {
    format!("{kind}-secret")
}

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

/// The kernel `shell` asks for, in the directory it names (`{workspace}` by default), with
/// the variables it sets (`env`) and those of the command's own environment it hands through
/// (`pass`). It starts from an empty environment otherwise.
fn shell_kernel_in(asked: &Asked<'_>, shell: ShellKernel) -> Built {
    asked.only(&["dir", "program", "env", "pass", "prefix"])?;
    let dir = asked
        .text("dir")
        .map_or_else(|| asked.places.workspace.clone(), PathBuf::from);
    let shell = asked.table("env")?.into_iter().fold(
        shell.in_dir(dir).with_prefix(asked.texts("prefix")?),
        |shell, (name, value)| shell.with_env(name, value),
    );
    let shell = asked
        .texts("pass")?
        .into_iter()
        .fold(shell, ShellKernel::passing);
    Ok(Box::new(shell_kernel_of(asked.name.to_owned(), shell)))
}

/// The MCP server `[mcp.<name>]` describes: a `command` started through a kernel (`shell` by
/// default), or a `url`, with the `secret` or the variable (`bearer_token_env`) its bearer
/// token comes from.
/// The page an entry of the kind [`PAGE`] asks for.
fn page_in(asked: &Asked<'_>) -> Built {
    asked.only(&["port"])?;
    let port = match asked.config.get("port") {
        None => 0,
        Some(port) => port
            .as_u64()
            .and_then(|port| u16::try_from(port).ok())
            .ok_or("`port` is a whole number from 0 (any free port) to 65535")?,
    };
    let config = corduroy_page::Config::new().port(port);
    Ok(Box::new(corduroy_page::page(asked.name, config)))
}

fn mcp_server_in(asked: &Asked<'_>) -> Built {
    asked.only(&[
        "command",
        "args",
        "env",
        "kernel",
        "url",
        "secret",
        "bearer_token_env",
        "timeout_seconds",
    ])?;
    let secret = asked.text("secret");
    if secret.is_some() && asked.text("url").is_none() {
        return Err("`secret` is the bearer token of a server at a `url`".to_owned());
    }
    if secret.is_some() && asked.text("bearer_token_env").is_some() {
        return Err("names both a `secret` and a `bearer_token_env`: give one".to_owned());
    }
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
    let server = match secret {
        Some(secret) => server.with_secret(secret),
        None => server,
    };
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
    let variable = env_variable(asked)?.to_owned();
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

/// The variable a `[secrets.<name>]` entry of the kind [`ENV_SECRET`] reads.
///
/// # Errors
///
/// If the entry has another setting, or no `env`.
pub fn env_variable<'a>(asked: &'a Asked<'_>) -> Result<&'a str, String> {
    asked.only(&["env"])?;
    asked
        .text("env")
        .ok_or_else(|| "`env` is required".to_owned())
}

/// The entry's name, if it can name what a person keeps: a sign-in's file, an item of a
/// keychain. `what` says which, for the error.
fn plain_name<'a>(asked: &Asked<'a>, what: &str) -> Result<&'a str, String> {
    let plain = |character: char| character.is_ascii_alphanumeric() || "-_".contains(character);
    if asked.name.is_empty() || !asked.name.chars().all(plain) {
        return Err(format!(
            "{what} is kept under its entry's name, which may hold only letters, digits, `-` \
             and `_`"
        ));
    }
    Ok(asked.name)
}

/// The name of the item a `[secrets.<name>]` entry of the kind [`KEYCHAIN_SECRET`] is kept
/// as: the entry's own, so that every agent of a person's that names it shares the value.
///
/// # Errors
///
/// If the entry has a setting (the kind takes none), or its name cannot name an item.
pub fn keychain_item<'a>(asked: &Asked<'a>) -> Result<&'a str, String> {
    asked.only(&[])?;
    plain_name(asked, "a secret in the keychain")
}

/// A `ChatGPT` sign-in as one item of a keychain. No entry's name holds a `.`, so the
/// item is never one a `[secrets.<name>]` entry of the kind [`KEYCHAIN_SECRET`] reads.
struct SignInItem {
    keychain: Arc<dyn Keychain>,
    item: String,
}

impl Vault for SignInItem {
    fn name(&self) -> String {
        self.keychain.name()
    }

    fn load(&self) -> Result<Option<String>, String> {
        self.keychain.get(&self.item)
    }

    fn save(&self, text: &str) -> Result<(), String> {
        self.keychain.set(&self.item, text)
    }

    fn remove(&self) -> Result<bool, String> {
        self.keychain.remove(&self.item)
    }
}

/// The `ChatGPT` sign-in `[secrets.<name>]` names.
///
/// It is the one kept under the entry's name, so that every agent of the person's that
/// names it shares it. It is kept in `keychain`, as the item `chatgpt-sign-in.<name>`,
/// unless the entry says `keep = "file"`: then in a file in the person's own directory.
/// The file is the choice to write down, and never what a machine without a keychain gets
/// by itself.
///
/// # Errors
///
/// If the entry has another setting, `keep` is neither, its name cannot name a file, or
/// there is no directory of the person's own.
pub fn chatgpt_in(asked: &Asked<'_>, keychain: &Arc<dyn Keychain>) -> Result<ChatGpt, String> {
    asked.only(&["keep"])?;
    let name = plain_name(asked, "a ChatGPT sign-in")?;
    let store = Store::named(&Places::person()?, name);
    let store = match asked.config.get("keep").map(Value::as_str) {
        None | Some(Some("keychain")) => store.kept_in(Arc::new(SignInItem {
            keychain: Arc::clone(keychain),
            item: format!("chatgpt-sign-in.{name}"),
        })),
        Some(Some("file")) => store,
        Some(_) => return Err("`keep` is \"keychain\" or \"file\"".to_owned()),
    };
    Ok(ChatGpt::new(store))
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
    /// | `shell-kernel` | `dir` (default `{workspace}`), `program` (default `sh`; any POSIX shell, such as `bash`, Git for Windows' included), `prefix` (a launcher argv), `env` (variables to set), `pass` (variables of the command's own environment to hand through) | a persistent shell as the kernel named like the entry, starting from an empty environment; what `[kernels.<name>]` with `kind = "shell"` builds |
    /// | `powershell-kernel` | `dir` (default `{workspace}`), `program` (default `powershell` on Windows, `pwsh` elsewhere), `prefix`, `env`, `pass` | a persistent PowerShell as the kernel named like the entry; what `[kernels.<name>]` with `kind = "powershell"` builds |
    /// | `http-client` | none | the client the `http` capability sends through, for `http` and `https` |
    /// | `page` | `port` (any free one by default) | the agent's page in a browser, served on `127.0.0.1` at an address the command says when it is about to run the agent: the record as it grows, a gate's question answered there, a prompt typed and a run stopped |
    /// | `mcp` | `command` (with `args`, `env` and `kernel`, `shell` by default) or `url` (with `secret` or `bearer_token_env`), and `timeout_seconds` | the tools of one MCP server, as `<name>__<tool>`; what `[mcp.<name>]` builds |
    /// | `env-secret` | `env` | the secret named like the entry, read from that environment variable when it is used; what `[secrets.<name>]` builds |
    /// | `keychain-secret` | none | the secret named like the entry, read from the system's keychain (the macOS Keychain, the Windows Credential Manager, the Secret Service) when it is first used; what `[secrets.<name>]` with `kind = "keychain"` builds |
    /// | `chatgpt-secret` | `keep` (`keychain`, the default, or `file`) | the secret named like the entry: the access token of a `ChatGPT` sign-in (`corduroy login`), renewed as it runs out; what `[secrets.<name>]` with `kind = "chatgpt"` builds |
    #[must_use]
    pub fn builtin() -> Self {
        let kinds = Self::none()
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
            .kind(PAGE, page_in)
            .kind(MCP, mcp_server_in);
        let system: Arc<dyn Keychain> = Arc::new(Os::new());
        kinds.kind(ENV_SECRET, env_secret_in).keeping_in(&system)
    }

    /// With the kinds that keep something in a keychain reading `keychain`:
    /// [`KEYCHAIN_SECRET`], and [`CHATGPT_SECRET`] for an entry that does not say `keep = "file"`.
    #[must_use]
    pub fn keeping_in(self, keychain: &Arc<dyn Keychain>) -> Self {
        let (items, sign_ins) = (Arc::clone(keychain), Arc::clone(keychain));
        self.kind(KEYCHAIN_SECRET, move |asked| {
            let item = keychain_item(asked)?;
            Ok(Box::new(keychain_secret(
                asked.name.to_owned(),
                Arc::clone(&items),
                item.to_owned(),
            )))
        })
        .kind(CHATGPT_SECRET, move |asked| {
            Ok(Box::new(chatgpt_secret(
                asked.name.to_owned(),
                chatgpt_in(asked, &sign_ins)?,
            )))
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
