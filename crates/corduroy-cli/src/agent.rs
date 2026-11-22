//! The agent file: which plugins an agent has and what each may do.
//!
//! An agent is described by a TOML file, `corduroy.toml` in the workspace by default. It names
//! the model, the providers, the kernels, the secrets plugins may use without seeing, and the
//! WebAssembly plugins with their grants. It is the first form of the grant files the design
//! calls for: one place to read, and edit, everything an agent can do.
//!
//! ```toml
//! provider = "anthropic"
//! model = "claude-sonnet-5-5"
//!
//! [providers.anthropic]
//! kind = "anthropic"                     # reads ANTHROPIC_API_KEY
//!
//! [kernels.shell]
//! kind = "shell"                         # a persistent shell in the workspace
//!
//! [[plugins]]
//! name = "pi-tools"
//! path = "pi_tools.wasm"
//! config = { kernel = "shell" }
//! grants = { fs-read = ["{workspace}", "{workspace}/*"], kernel = "shell" }
//! ```
//!
//! MCP servers go under `[mcp.<name>]`, with a `command` (started through a kernel, `shell`
//! by default) or a `url`; their tools are named `<name>__<tool>`:
//!
//! ```toml
//! [mcp.github]
//! command = "npx"
//! args = ["-y", "@modelcontextprotocol/server-github"]
//! env = { GITHUB_PERSONAL_ACCESS_TOKEN = "..." }
//! ```
//!
//! Strings may use `{workspace}` (where the agent works), `{data}` (its `.corduroy`
//! directory) and `{home}`. A grant is a pattern, a list of patterns, or
//! `{ allow = [..], deny = [..] }`; `*` matches any run of characters, `/` included. A plugin
//! is linked only to the capabilities it imports, and is not started at all if one of them
//! is not granted here.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use corduroy_harness::core::{FiberState, SECRETS, Scope, Secret, Secrets, Spawn, plugin_fn};
use corduroy_harness::wasm::{CAPABILITIES, WasmRuntime};
use corduroy_harness::{Harness, jsonl, provider, shell_kernel};
use corduroy_mcp::{McpPlugin, Transport, mcp_server};
use corduroy_providers::ProviderSpec;
use serde::Deserialize;
use serde_json::Value;

/// The file an agent is read from when none is named.
pub const AGENT_FILE: &str = "corduroy.toml";

/// The agent's own directory in the workspace: its log, compiled-code cache and skills.
pub const DATA_DIR: &str = ".corduroy";

/// An agent file, as written.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentFile {
    /// The start of every system prompt; plugins' prompt parts follow it.
    #[serde(default)]
    pub system: Option<String>,
    /// The provider the loop asks, one of `providers`.
    pub provider: String,
    /// The model to ask it for.
    pub model: String,
    /// Model turns a run may take without answering.
    #[serde(default)]
    pub max_turns: Option<u32>,
    /// The event log, `{data}/log.jsonl` by default.
    #[serde(default)]
    pub log: Option<String>,
    /// Model providers, by the name the agent refers to them by.
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderEntry>,
    /// Kernels, by name.
    #[serde(default)]
    pub kernels: BTreeMap<String, KernelEntry>,
    /// Secrets WebAssembly plugins may use through the `secret` capability, by name.
    #[serde(default)]
    pub secrets: BTreeMap<String, SecretEntry>,
    /// MCP servers, by the name their tools are prefixed with.
    #[serde(default)]
    pub mcp: BTreeMap<String, McpEntry>,
    /// WebAssembly plugins, loaded in order.
    #[serde(default)]
    pub plugins: Vec<PluginEntry>,
}

/// A model provider.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderEntry {
    /// What to build and how to reach it.
    #[serde(flatten)]
    pub spec: ProviderSpec,
    /// Models of this provider plugins may ask through the `llm` capability.
    #[serde(default)]
    pub serve: Vec<String>,
}

/// A kernel.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KernelEntry {
    /// `shell`: a persistent POSIX shell.
    pub kind: String,
    /// The directory it starts in, `{workspace}` by default.
    #[serde(default)]
    pub dir: Option<String>,
}

/// A secret, read from the environment when a plugin's request needs it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretEntry {
    /// The environment variable holding the value.
    pub env: String,
}

/// An MCP server: a `command` started through a kernel, or a `url`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpEntry {
    /// The program to start, speaking MCP over its standard input and output.
    #[serde(default)]
    pub command: Option<String>,
    /// Its arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables to set for it.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// The kernel that starts it, `shell` by default.
    #[serde(default)]
    pub kernel: Option<String>,
    /// Where a server reached over streamable HTTP is.
    #[serde(default)]
    pub url: Option<String>,
    /// The environment variable holding a bearer token for `url`.
    #[serde(default)]
    pub bearer_token_env: Option<String>,
    /// Seconds to wait for it to start and for each tool call.
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

/// A WebAssembly plugin and what it may do.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginEntry {
    /// The name it is loaded under.
    pub name: String,
    /// The component: absolute, relative to the agent file, or found on the plugin path.
    pub path: String,
    /// Its configuration, passed to `activate` as JSON.
    #[serde(default)]
    pub config: Option<toml::Value>,
    /// Each capability it is granted, with its scope.
    #[serde(default)]
    pub grants: BTreeMap<String, Grant>,
    /// Set to `false` to keep the entry without loading the plugin.
    #[serde(default = "yes")]
    pub enabled: bool,
}

const fn yes() -> bool {
    true
}

/// The scope of one grant.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Grant {
    /// One pattern.
    One(String),
    /// Any of these patterns.
    Many(Vec<String>),
    /// These patterns, except those.
    Scoped {
        /// Patterns reached.
        allow: Vec<String>,
        /// Patterns never reached, whatever `allow` says.
        #[serde(default)]
        deny: Vec<String>,
    },
}

/// Why an agent could not be read or put together.
#[derive(Debug)]
pub struct AgentError(String);

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AgentError {}

impl AgentError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

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

    /// `text` with `{workspace}`, `{data}` and `{home}` filled in.
    #[must_use]
    pub fn expand(&self, text: &str) -> String {
        let home_dir = std::env::var("HOME").unwrap_or_default();
        text.replace("{workspace}", &self.workspace.to_string_lossy())
            .replace("{data}", &self.data.to_string_lossy())
            .replace("{home}", &home_dir)
    }

    fn expand_json(&self, value: Value) -> Value {
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

    /// The component a plugin entry names.
    fn find(&self, path: &str) -> Result<PathBuf, AgentError> {
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
                AgentError::new(format!(
                    "plugin `{}` not found in {}. Build the plugins (`cargo plugins`, \
                     `scripts/guests.sh`) or set CORDUROY_PLUGIN_PATH",
                    path.display(),
                    searched.join(", ")
                ))
            })
    }
}

impl AgentFile {
    /// Parse an agent file.
    ///
    /// # Errors
    ///
    /// Returns what is wrong with the text, with its line.
    pub fn parse(text: &str) -> Result<Self, AgentError> {
        let file: Self = toml::from_str(text)
            .map_err(|error| AgentError::new(format!("agent file: {error}")))?;
        file.check()?;
        Ok(file)
    }

    /// Read and parse the agent file at `path`.
    ///
    /// # Errors
    ///
    /// As [`parse`](Self::parse), or if the file cannot be read.
    pub fn read(path: &Path) -> Result<Self, AgentError> {
        let text = fs::read_to_string(path)
            .map_err(|error| AgentError::new(format!("{}: {error}", path.display())))?;
        Self::parse(&text).map_err(|error| AgentError::new(format!("{}: {error}", path.display())))
    }

    fn check(&self) -> Result<(), AgentError> {
        if !self.providers.contains_key(&self.provider) {
            return Err(AgentError::new(format!(
                "`provider = \"{}\"` names no [providers.{}] entry",
                self.provider, self.provider
            )));
        }
        let unknown = self.plugins.iter().find_map(|plugin| {
            plugin
                .grants
                .keys()
                .find(|capability| !CAPABILITIES.contains(&capability.as_str()))
                .map(|capability| (&plugin.name, capability))
        });
        if let Some((plugin, capability)) = unknown {
            return Err(AgentError::new(format!(
                "plugin `{plugin}` is granted `{capability}`, which is not a capability (known: {})",
                CAPABILITIES.join(", ")
            )));
        }
        for (name, server) in &self.mcp {
            let kernel = server.kernel.as_deref().unwrap_or("shell");
            match (&server.command, &server.url) {
                (Some(_), None) if !self.kernels.contains_key(kernel) => {
                    return Err(AgentError::new(format!(
                        "MCP server `{name}` is started by the kernel `{kernel}`, which has no \
                         [kernels.{kernel}] entry"
                    )));
                }
                (Some(_), None) | (None, Some(_)) => {}
                _ => {
                    return Err(AgentError::new(format!(
                        "MCP server `{name}` needs a `command` or a `url`, not both"
                    )));
                }
            }
        }
        for (name, kernel) in &self.kernels {
            if kernel.kind != "shell" {
                return Err(AgentError::new(format!(
                    "kernel `{name}` is of kind `{}`; the only kind is `shell`",
                    kernel.kind
                )));
            }
        }
        Ok(())
    }

    /// The default agent for a workspace with no agent file: the model from the environment
    /// (`ANTHROPIC_API_KEY`, then `OPENAI_API_KEY`, else a local Ollama), the Pi toolset on a
    /// shell in the workspace, the approval and inspect gates, and the Goose-baseline
    /// plugins (skills, hints, todo, memory, recall, summarize, compact).
    #[must_use]
    pub fn default_text() -> String {
        let (kind, model) = if std::env::var_os("ANTHROPIC_API_KEY").is_some() {
            ("anthropic", "claude-sonnet-5-5")
        } else if std::env::var_os("OPENAI_API_KEY").is_some() {
            ("openai", "gpt-5")
        } else {
            ("ollama", "qwen3")
        };
        DEFAULT
            .replace("{{kind}}", kind)
            .replace("{{model}}", model)
    }
}

/// The agent `corduroy init` writes, and the one used when there is no agent file.
const DEFAULT: &str = r#"# A corduroy agent. Everything it can do is listed here; edit to change it.
# Strings may use {workspace}, {data} (the .corduroy directory) and {home}.

system = """
You are a careful coding agent working in {workspace}. Read before you change anything, \
keep changes small, and say what you did."""
provider = "main"
model = "{{model}}"
max_turns = 50

[providers.main]
kind = "{{kind}}"   # anthropic, openai, ollama, *-compatible, or a provider oi-kit knows
serve = ["{{model}}"]   # models plugins may ask through the `llm` capability

[kernels.shell]
kind = "shell"      # a persistent shell, starting in {workspace}

# read, write, edit and bash, within the workspace. The agent file and the log are out of
# reach whatever this says.
[[plugins]]
name = "pi-tools"
path = "pi_tools.wasm"
config = { kernel = "shell" }
[plugins.grants]
fs-read = ["{workspace}", "{workspace}/*"]
fs-write = { allow = ["{workspace}", "{workspace}/*"], deny = ["{workspace}/.git/*"] }
kernel = "shell"

# Procedures the agent saves for itself.
[[plugins]]
name = "skills"
path = "skills.wasm"
config = { dir = "{data}/skills" }
[plugins.grants]
fs-read = ["{data}/skills", "{data}/skills/*"]
fs-write = ["{data}/skills", "{data}/skills/*"]
events = "skills/*"

# Asks you before any tool that changes something: auto, smart, approve or chat. Tools that
# only change the agent's own notes are allowed.
[[plugins]]
name = "approval"
path = "approval.wasm"
config = { mode = "smart", allow = ["todo_write", "memory_save", "skill_manage"] }

# Asks you about destructive commands and credentials in a call, stops a model repeating
# itself, and warns it when a tool's output tries to give it instructions.
[[plugins]]
name = "inspect"
path = "inspect.wasm"

# AGENTS.md, .goosehints and CLAUDE.md in the workspace, in the system prompt.
[[plugins]]
name = "hints"
path = "hints.wasm"
config = { files = ["{workspace}/AGENTS.md", "{workspace}/.goosehints", "{workspace}/CLAUDE.md"] }
grants = { fs-read = ["{workspace}/AGENTS.md", "{workspace}/.goosehints", "{workspace}/CLAUDE.md"] }

# A todo list the agent keeps across turns and restarts.
[[plugins]]
name = "todo"
path = "todo.wasm"
grants = { state = "*" }

# Memories by category, as Markdown files you can read and edit.
[[plugins]]
name = "memory"
path = "memory.wasm"
config = { dir = "{data}/memory" }
grants = { fs-read = ["{data}/memory", "{data}/memory/*"], fs-write = ["{data}/memory", "{data}/memory/*"] }

# Search earlier sessions in the log.
[[plugins]]
name = "recall"
path = "recall.wasm"
grants = { log-read = ["*:message", "*:session/start"] }

# Files read and summarized by a model of its own.
[[plugins]]
name = "summarize"
path = "summarize.wasm"
config = { model = "{{model}}" }
grants = { fs-read = ["{workspace}", "{workspace}/*"], llm = "{{model}}" }

# Condenses the start of a long conversation into a summary once a request passes
# max_tokens; the messages stay in the log.
[[plugins]]
name = "compact"
path = "compact.wasm"
config = { model = "{{model}}", max_tokens = 100000 }
grants = { llm = "{{model}}" }

# Lets the model load more plugins from the plugin directory, never granting more than this
# entry holds. Off by default.
[[plugins]]
name = "plugin-manager"
path = "plugin_manager.wasm"
enabled = false
config = { dirs = ["{data}/plugins"] }
grants = { fs-read = ["{data}/plugins", "{data}/plugins/*"], plugins = "*", state = "*" }
"#;

impl Grant {
    fn scope(&self, places: &Places) -> Scope {
        let (allow, deny): (Vec<&String>, Vec<&String>) = match self {
            Self::One(pattern) => (vec![pattern], Vec::new()),
            Self::Many(patterns) => (patterns.iter().collect(), Vec::new()),
            Self::Scoped { allow, deny } => (allow.iter().collect(), deny.iter().collect()),
        };
        let scope = allow.into_iter().fold(Scope::none(), |scope, pattern| {
            scope.allow(places.expand(pattern))
        });
        deny.into_iter()
            .fold(scope, |scope, pattern| scope.deny(places.expand(pattern)))
    }
}

/// An agent put together: the harness, and what went wrong loading its plugins.
#[derive(Debug)]
pub struct Agent {
    /// The harness, with every plugin loaded.
    pub harness: Harness,
    /// What it was put together from.
    pub file: AgentFile,
    /// Where the event log is.
    pub log: PathBuf,
    /// Plugins that did not start.
    pub failed: Vec<Unstarted>,
}

/// A plugin that was loaded but did not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unstarted {
    /// Its name.
    pub plugin: String,
    /// Why not.
    pub reason: String,
}

/// Put the agent `file` describes together in `places`.
///
/// Opens its log, builds its providers and kernels, registers its secrets, and loads its
/// plugins with their grants. The log, the agent file and the compiled-code cache are out of
/// reach of every plugin's `fs-read` and `fs-write`, so a plugin cannot change its own grants
/// or rewrite the record.
///
/// # Errors
///
/// Returns an [`AgentError`] if the log cannot be opened, a provider cannot be built or a
/// plugin cannot be found or compiled. A plugin that is loaded but does not start is
/// reported in [`Agent::failed`] instead.
pub fn assemble(file: &AgentFile, places: &Places) -> Result<Agent, AgentError> {
    fs::create_dir_all(&places.data)
        .map_err(|error| AgentError::new(format!("{}: {error}", places.data.display())))?;
    let log = file.log.as_deref().map_or_else(
        || places.data.join("log.jsonl"),
        |path| PathBuf::from(places.expand(path)),
    );
    let events = jsonl::open(&log)
        .map_err(|error| AgentError::new(format!("log {}: {error}", log.display())))?;
    let mut harness = Harness::open(Arc::new(events))
        .map_err(|error| AgentError::new(error.to_string()))?
        .with_provider(file.provider.clone())
        .with_model(&file.model);
    if let Some(system) = &file.system {
        harness = harness.with_system_prompt(places.expand(system));
    }
    if let Some(max_turns) = file.max_turns {
        harness = harness.with_max_turns(max_turns);
    }
    natives(&harness, file, places)?;
    let failed = plugins(&harness, file, places, &log)?;
    Ok(Agent {
        harness,
        file: file.clone(),
        log,
        failed,
    })
}

/// The native plugins: an HTTP client, the providers, the kernels and the secrets.
fn natives(harness: &Harness, file: &AgentFile, places: &Places) -> Result<(), AgentError> {
    harness.plugin(corduroy_http::http_client());
    for (name, entry) in &file.providers {
        let built = corduroy_providers::build(name, &entry.spec)
            .map_err(|error| AgentError::new(error.to_string()))?;
        harness.plugin(provider(name.clone(), built).serving(entry.serve.clone()));
    }
    for (name, kernel) in &file.kernels {
        let dir = kernel.dir.as_deref().map_or_else(
            || places.workspace.clone(),
            |dir| PathBuf::from(places.expand(dir)),
        );
        harness.plugin(shell_kernel(name.clone(), dir));
    }
    for (name, server) in &file.mcp {
        harness.plugin(mcp(name, server, places)?);
    }
    let secrets: Vec<(String, String)> = file
        .secrets
        .iter()
        .map(|(name, secret)| (name.clone(), secret.env.clone()))
        .collect();
    harness.plugin(plugin_fn("secrets", &[SECRETS], move |ctx| {
        let registry = ctx.service::<Secrets>(SECRETS)?;
        for (name, variable) in &secrets {
            let variable = variable.clone();
            let secret: Arc<dyn Secret> = Arc::new(move || {
                std::env::var(&variable).map_err(|_| format!("${variable} is not set"))
            });
            registry.register(ctx, name.clone(), secret)?;
        }
        Ok(())
    }));
    Ok(())
}

/// The plugin for the MCP server `name`.
fn mcp(name: &str, server: &McpEntry, places: &Places) -> Result<McpPlugin, AgentError> {
    let transport = match (&server.command, &server.url) {
        (Some(command), _) => Transport::Stdio {
            kernel: server.kernel.clone().unwrap_or_else(|| "shell".to_owned()),
            spawn: Spawn {
                program: places.expand(command),
                args: server.args.iter().map(|arg| places.expand(arg)).collect(),
                env: server
                    .env
                    .iter()
                    .map(|(key, value)| (key.clone(), places.expand(value)))
                    .collect(),
            },
        },
        (None, url) => {
            let bearer_token = server
                .bearer_token_env
                .as_ref()
                .map(|variable| {
                    std::env::var(variable).map_err(|_| {
                        AgentError::new(format!("MCP server `{name}`: ${variable} is not set"))
                    })
                })
                .transpose()?;
            Transport::Http {
                url: places.expand(url.as_deref().unwrap_or_default()),
                bearer_token,
            }
        }
    };
    let plugin = mcp_server(name, transport);
    Ok(match server.timeout_seconds {
        Some(seconds) => plugin.with_timeout(Duration::from_secs(seconds)),
        None => plugin,
    })
}

/// The WebAssembly plugins, each with its grants. Returns those that did not start.
fn plugins(
    harness: &Harness,
    file: &AgentFile,
    places: &Places,
    log: &Path,
) -> Result<Vec<Unstarted>, AgentError> {
    let mut runtime = WasmRuntime::cached(places.data.join("cache"))
        .map_err(|error| AgentError::new(error.to_string()))?
        .with_root(&places.workspace)
        .protect(log);
    if let Some(path) = &places.file {
        runtime = runtime.protect(path);
    }
    let mut loaded = Vec::new();
    for entry in file.plugins.iter().filter(|entry| entry.enabled) {
        let failed =
            |error: &dyn fmt::Display| AgentError::new(format!("plugin `{}`: {error}", entry.name));
        let component = places.find(&entry.path)?;
        let config = match &entry.config {
            Some(config) => {
                places.expand_json(serde_json::to_value(config).map_err(|error| failed(&error))?)
            }
            None => Value::Object(serde_json::Map::new()),
        };
        let mut plugin = runtime
            .load(&component)
            .map_err(|error| failed(&error))?
            .named(entry.name.clone())
            .config(config.to_string());
        for (capability, grant) in &entry.grants {
            plugin = plugin.grant(capability.clone(), grant.scope(places));
        }
        loaded.push((entry.name.clone(), harness.plugin(plugin)));
    }
    let app = harness.app();
    Ok(loaded
        .into_iter()
        .filter_map(|(plugin, id)| {
            let reason = match app.state(id)? {
                FiberState::Failed(reason) => reason.clone(),
                FiberState::Pending => "it is waiting for a service".to_owned(),
                FiberState::Active => return None,
            };
            Some(Unstarted { plugin, reason })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_agent_parses() {
        let file = AgentFile::parse(&AgentFile::default_text()).unwrap();
        assert_eq!(file.provider, "main");
        assert_eq!(file.plugins.len(), 11);
        assert_eq!(
            file.plugins.iter().filter(|plugin| plugin.enabled).count(),
            10
        );
        assert!(file.plugins[0].grants.contains_key("kernel"));
    }

    #[test]
    fn mistakes_are_named() {
        let unknown_provider = "provider = \"x\"\nmodel = \"m\"\n";
        assert!(
            AgentFile::parse(unknown_provider)
                .unwrap_err()
                .to_string()
                .contains("[providers.x]")
        );
        let bad_grant = "provider = \"p\"\nmodel = \"m\"\n[providers.p]\nkind = \"ollama\"\n\
                         [[plugins]]\nname = \"a\"\npath = \"a.wasm\"\ngrants = { fs = \"*\" }\n";
        let error = AgentFile::parse(bad_grant).unwrap_err().to_string();
        assert!(error.contains("`fs`, which is not a capability"), "{error}");
        let typo =
            "provider = \"p\"\nmodel = \"m\"\nmax_turn = 3\n[providers.p]\nkind = \"ollama\"\n";
        assert!(
            AgentFile::parse(typo)
                .unwrap_err()
                .to_string()
                .contains("max_turn")
        );
    }

    #[test]
    fn grants_expand_their_places() {
        let places = Places::new(PathBuf::from("/work"), None);
        let grant = Grant::Scoped {
            allow: vec!["{workspace}/*".into()],
            deny: vec!["{workspace}/.git/*".into()],
        };
        let scope = grant.scope(&places);
        assert!(scope.permits("/work/src/main.rs"));
        assert!(!scope.permits("/work/.git/config"));
        assert!(!scope.permits("/elsewhere"));
        assert_eq!(places.expand("{data}/skills"), "/work/.corduroy/skills");
    }
}
