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
//! Extensions (native plugins of a kind this command was built with, see
//! [`extension`](crate::extension)) go under `[extensions.<name>]`; `kind` names the kind and
//! every other key is the extension's own configuration. Kernels, MCP servers and secrets are
//! extensions too, built from kinds the same way: `[kernels.<name>]` with `kind = "shell"`
//! builds the kind `shell-kernel` with the entry's other keys (`dir`, `program`) as its
//! configuration, `[mcp.<name>]` the kind `mcp` and `[secrets.<name>]` the kind `env-secret`:
//!
//! ```toml
//! [extensions.scratch]
//! kind = "shell-kernel"
//! dir = "{workspace}/scratch"
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

use corduroy_harness::core::{FiberId, FiberState, ModelInfo, Scope};
use corduroy_harness::wasm::{CAPABILITIES, WORLD, WasmRuntime};
use corduroy_harness::{Harness, ProcessInfo, provider};
use corduroy_providers::ProviderSpec;
pub use corduroy_std::{DATA_DIR, Places};
use corduroy_std::{ENV_SECRET, MCP, Standard, kernel_kind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::extension::{Asked, Extensions};

/// The file an agent is read from when none is named.
pub const AGENT_FILE: &str = "corduroy.toml";

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
    /// Extensions: native plugins of a kind this command was built with, by name. Loaded
    /// after the kernels, providers and MCP servers and before the WebAssembly plugins.
    #[serde(default)]
    pub extensions: BTreeMap<String, ExtensionEntry>,
    /// WebAssembly plugins, loaded in order.
    #[serde(default)]
    pub plugins: Vec<PluginEntry>,
}

/// An extension: a native plugin of one of the kinds the command was built with.
#[derive(Debug, Clone, Deserialize)]
pub struct ExtensionEntry {
    /// Which kind of extension to build.
    pub kind: String,
    /// The extension's own configuration: every other key of the entry.
    #[serde(flatten)]
    pub config: BTreeMap<String, toml::Value>,
}

/// A model provider.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "toml::Table")]
pub struct ProviderEntry {
    /// What to build and how to reach it: every key of the entry but `models`.
    pub spec: ProviderSpec,
    /// What the file states or corrects about models of this provider, by the provider's
    /// name for each. What a provider reports about a model stands where this says nothing.
    pub models: BTreeMap<String, ModelEntry>,
}

impl TryFrom<toml::Table> for ProviderEntry {
    type Error = toml::de::Error;

    /// Taken apart by hand, not flattened, so that a key neither part knows is an error.
    fn try_from(mut entry: toml::Table) -> Result<Self, Self::Error> {
        if entry.contains_key("serve") {
            return Err(serde::de::Error::custom(
                "`serve` is not a key of a provider any more: an add-on's `llm` grant says \
                 which models it may ask, each as `<provider>/<model>`",
            ));
        }
        let models = entry
            .remove("models")
            .map(toml::Value::try_into)
            .transpose()?
            .unwrap_or_default();
        let spec = toml::Value::Table(entry).try_into()?;
        Ok(Self { spec, models })
    }
}

/// What an agent file says about one model: any of what a model's description holds. A model
/// served on this machine often reports nothing, so this is where its context is stated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    /// How many tokens the model holds at once.
    #[serde(default)]
    pub context_tokens: Option<u64>,
    /// The most tokens it may write in one answer.
    #[serde(default)]
    pub output_tokens: Option<u64>,
    /// Whether it takes images.
    #[serde(default)]
    pub images: Option<bool>,
    /// Whether it can be offered tools.
    #[serde(default)]
    pub tools: Option<bool>,
}

impl ModelEntry {
    /// The entry as a model's description, unknown where the file says nothing.
    const fn info(self) -> ModelInfo {
        let mut info = ModelInfo::unknown();
        info.context_tokens = self.context_tokens;
        info.output_tokens = self.output_tokens;
        info.images = self.images;
        info.tools = self.tools;
        info
    }
}

/// A kernel: an extension of the kind `<kind>-kernel`, such as `shell-kernel`.
#[derive(Debug, Clone, Deserialize)]
pub struct KernelEntry {
    /// `shell` (a persistent POSIX shell, `program` `sh` by default) or `powershell`
    /// (`powershell` on Windows, `pwsh` elsewhere), starting in `dir` (`{workspace}` by
    /// default), unless the command brings other kinds.
    pub kind: String,
    /// The kernel's own configuration: every other key of the entry. The kind refuses a key
    /// it does not know.
    #[serde(flatten)]
    pub config: BTreeMap<String, toml::Value>,
}

/// A secret, read from the environment when a plugin's request needs it: an extension of the
/// kind `env-secret`, with `env` as its configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecretEntry {
    /// The environment variable holding the value.
    pub env: String,
}

/// An MCP server: a `command` started through a kernel, or a `url`. An extension of the kind
/// `mcp`, with the entry's keys as its configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpEntry {
    /// The program to start, speaking MCP over its standard input and output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Its arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Environment variables to set for it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// The kernel that starts it, `shell` by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
    /// Where a server reached over streamable HTTP is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The environment variable holding a bearer token for `url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_token_env: Option<String>,
    /// Seconds to wait for it to start and for each tool call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

/// A WebAssembly plugin and what it may do.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginEntry {
    /// The name it is loaded under.
    pub name: String,
    /// The component: absolute, or found on the plugin path, which ends with the agent
    /// file's directory.
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

impl AgentFile {
    /// Where the event log is kept: `log`, or `log.jsonl` in the agent's own directory. A
    /// relative path is in the workspace, like every other path the file names.
    #[must_use]
    pub fn log_path(&self, places: &Places) -> PathBuf {
        self.log.as_deref().map_or_else(
            || places.data.join("log.jsonl"),
            |path| places.workspace.join(places.expand(path)),
        )
    }

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
        Ok(())
    }

    /// The default agent for a workspace with no agent file: the model from the environment
    /// (`ANTHROPIC_API_KEY`, then `OPENAI_API_KEY`, else a local Ollama), the Pi toolset on a
    /// shell in the workspace (PowerShell on Windows, told to the model, `sh` elsewhere), the
    /// approval and inspect gates, the Goose-baseline plugins (skills, hints, todo, memory,
    /// recall, summarize, compact, subagents) and `review`. With both keys set, `review` asks
    /// the other vendor's model, from a second provider named `reviewer`.
    #[must_use]
    pub fn default_text() -> String {
        let set = |key: &str| std::env::var_os(key).is_some();
        default_for(set("ANTHROPIC_API_KEY"), set("OPENAI_API_KEY"))
    }
}

/// The provider kind and model the default agent uses for each vendor.
const ANTHROPIC: (&str, &str) = ("anthropic", "claude-sonnet-5-5");
const OPENAI: (&str, &str) = ("openai", "gpt-5");

/// The default agent when the keys for Anthropic and for `OpenAI` are, or are not, set.
fn default_for(anthropic: bool, openai: bool) -> String {
    let (kind, model) = if anthropic {
        ANTHROPIC
    } else if openai {
        OPENAI
    } else {
        ("ollama", "qwen3")
    };
    // With both keys, the vendor the loop does not use reviews its work.
    let (reviewer, review_model, review_said) = if anthropic && openai {
        let (kind, model) = OPENAI;
        (
            format!(
                "\n# A second provider, so another vendor's model reviews what the main one \
                 writes.\n[providers.reviewer]\nkind = \"{kind}\"\n"
            ),
            format!("reviewer/{model}"),
            "Another vendor's model reviews",
        )
    } else {
        (
            String::new(),
            format!("main/{model}"),
            "The same model, with fresh eyes, reviews",
        )
    };
    let (shell, said) = if cfg!(windows) {
        ("powershell", " Commands run in PowerShell.")
    } else {
        ("shell", "")
    };
    DEFAULT
        .replace("{{reviewer}}", &reviewer)
        .replace("{{review_said}}", review_said)
        .replace("{{review_model}}", &review_model)
        .replace("{{kind}}", kind)
        .replace("{{model}}", model)
        .replace("{{shell}}", shell)
        .replace("{{shell_said}}", said)
}

/// The agent `corduroy init` writes, and the one used when there is no agent file.
const DEFAULT: &str = r#"# A corduroy agent. Everything it can do is listed here; edit to change it.
# Strings may use {workspace}, {data} (the .corduroy directory) and {home}.

system = """
You are a careful coding agent working in {workspace}. Read before you change anything, \
keep changes small, and say what you did.{{shell_said}}"""
provider = "main"
model = "{{model}}"
max_turns = 50

[providers.main]
kind = "{{kind}}"   # anthropic, openai, ollama, *-compatible, or a provider oi-kit knows
# max_tokens = 16000   # the most one answer may take; by default, the model's own limit
# thinking = "medium"  # off, low, medium, high or max, for a model that can think
{{reviewer}}
[kernels.shell]
kind = "{{shell}}"      # a persistent shell in {workspace}: shell (sh) or powershell

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
config = { mode = "smart", allow = ["todo_write", "memory_save", "skill_manage", "delegate"] }

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

# A todo list for each session, kept across turns and restarts.
[[plugins]]
name = "todo"
path = "todo.wasm"
grants = { state = "*", session = "*" }

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

# Files read and summarized by a model of its own. An add-on names a model as
# <provider>/<model>, and its llm grant says which models it may ask.
[[plugins]]
name = "summarize"
path = "summarize.wasm"
config = { model = "main/{{model}}" }
grants = { fs-read = ["{workspace}", "{workspace}/*"], llm = "main/{{model}}" }

# {{review_said}} the changes in the workspace (git diff and new files) and says
# what is wrong, for the agent to fix before it reports the work done. To have another model
# review, name it here and in the llm grant, as <provider>/<model>.
[[plugins]]
name = "review"
path = "review.wasm"
config = { model = "{{review_model}}", kernel = "shell", dir = "{workspace}" }
grants = { fs-read = ["{workspace}", "{workspace}/*"], kernel = "shell", llm = "{{review_model}}" }

# Hands a task to a sub-agent with the same tools and its own conversation, and waits for
# its answer. The sub-agent's own tool calls pass the same gates.
[[plugins]]
name = "subagents"
path = "subagents.wasm"
grants = { service = "sessions" }

# Condenses the start of a long conversation into a summary once a request passes three
# quarters of what the model holds (100000 tokens if the model does not say; set max_tokens
# to choose). The messages stay in the log.
[[plugins]]
name = "compact"
path = "compact.wasm"
config = { model = "main/{{model}}" }
grants = { llm = "main/{{model}}" }

# Lets the model load more plugins from the plugin directory, never granting more than this
# entry holds. Off by default.
[[plugins]]
name = "plugin-manager"
path = "plugin_manager.wasm"
enabled = false
config = { dirs = ["{data}/plugins"] }
grants = { fs-read = ["{data}/plugins", "{data}/plugins/*"], plugins = "*", state = "*", session = "*" }
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

/// Put the agent `file` describes together in `places`, from the standard extensions. See
/// [`assemble_from`].
///
/// # Errors
///
/// As [`assemble_from`].
pub fn assemble(file: &AgentFile, places: &Places) -> Result<Agent, AgentError> {
    assemble_from(file, places, &Standard::new())
}

/// Put the agent `file` describes together in `places`, from the standard extensions with the
/// kinds in `extensions` added. See [`assemble_from`].
///
/// # Errors
///
/// As [`assemble_from`].
pub fn assemble_with(
    file: &AgentFile,
    places: &Places,
    extensions: &Extensions,
) -> Result<Agent, AgentError> {
    assemble_from(file, places, &Standard::new().with_kinds(extensions))
}

/// Put the agent `file` describes together in `places`, from the parts in `standard`.
///
/// Opens its log, loads the extensions every agent gets, builds its providers and kernels,
/// registers its secrets, and loads its extensions and then its plugins with their grants.
/// The log, the agent file and the compiled-code cache are out of reach of every plugin's
/// `fs-read` and `fs-write`, so a plugin cannot change its own grants or rewrite the record.
///
/// # Errors
///
/// Returns an [`AgentError`] if the log cannot be opened, a provider cannot be built, a
/// kernel or extension is of a kind `standard` does not have or cannot be built, a plugin is
/// listed while `standard` loads no add-ons, or a plugin cannot be found or compiled. A
/// plugin that is loaded but does not start is reported in [`Agent::failed`] instead.
pub fn assemble_from(
    file: &AgentFile,
    places: &Places,
    standard: &Standard,
) -> Result<Agent, AgentError> {
    fs::create_dir_all(&places.data)
        .map_err(|error| AgentError::new(format!("{}: {error}", places.data.display())))?;
    let log = file.log_path(places);
    let events = standard
        .open_log(&log)
        .map_err(|error| AgentError::new(format!("log {}: {error}", log.display())))?;
    // What this process runs as, before anything else it writes.
    let platform = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
    let mut process = ProcessInfo::new("corduroy", env!("CARGO_PKG_VERSION"), platform);
    if standard.addons() {
        process = process.world(WORLD);
    }
    events
        .emit(process.start())
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
    let mut failed = natives(&harness, file, places, standard)?;
    failed.extend(extend(&harness, file, places, standard)?);
    failed.extend(plugins(&harness, file, places, standard, &log)?);
    Ok(Agent {
        harness,
        file: file.clone(),
        log,
        failed,
    })
}

/// Build the extension `name` of `kind` from `config` and load it. `what` names the entry in
/// an error.
fn load(
    harness: &Harness,
    places: &Places,
    kinds: &Extensions,
    entry: (&str, &str, &str),
    config: Value,
) -> Result<FiberId, AgentError> {
    let (what, name, kind) = entry;
    let failed = |error: &dyn fmt::Display| AgentError::new(format!("{what}: {error}"));
    let config = places.expand_json(config);
    let asked = Asked {
        name,
        config: &config,
        places,
    };
    let plugin = kinds
        .build(kind, &asked)
        .ok_or_else(|| {
            failed(&format!(
                "this command has no extension kind `{kind}` (it has: {})",
                kinds.kinds().join(", ")
            ))
        })?
        .map_err(|reason| failed(&reason))?;
    Ok(harness.plugin(plugin))
}

/// The native plugins: the extensions every agent gets, the providers, the kernels, the MCP
/// servers and the secrets, in that order. Returns those that did not start, and the
/// providers that could not be built but that the agent can do without.
fn natives(
    harness: &Harness,
    file: &AgentFile,
    places: &Places,
    standard: &Standard,
) -> Result<Vec<Unstarted>, AgentError> {
    let kinds = standard.kinds();
    let mut loaded = Vec::new();
    for kind in standard.defaults() {
        let entry = (
            &*format!("extension `{kind}`"),
            kind.as_str(),
            kind.as_str(),
        );
        let id = load(
            harness,
            places,
            kinds,
            entry,
            Value::Object(serde_json::Map::new()),
        )?;
        loaded.push((format!("extension {kind}"), id));
    }
    let mut failed = providers(harness, file)?;
    for (name, kernel) in &file.kernels {
        let what = format!("kernel `{name}`");
        let kind = kernel_kind(&kernel.kind);
        if !kinds.has(&kind) {
            return Err(AgentError::new(format!(
                "{what} is of kind `{}`, which this command does not have (it has: {})",
                kernel.kind,
                kinds.kernel_kinds().join(", ")
            )));
        }
        let config = serde_json::to_value(&kernel.config)
            .map_err(|error| AgentError::new(format!("{what}: {error}")))?;
        let id = load(harness, places, kinds, (&what, name, &kind), config)?;
        loaded.push((format!("kernel {name}"), id));
    }
    for (name, server) in &file.mcp {
        let what = format!("MCP server `{name}`");
        let config = serde_json::to_value(server)
            .map_err(|error| AgentError::new(format!("{what}: {error}")))?;
        let id = load(harness, places, kinds, (&what, name, MCP), config)?;
        loaded.push((format!("mcp {name}"), id));
    }
    for (name, secret) in &file.secrets {
        let what = format!("secret `{name}`");
        let config = serde_json::to_value(secret)
            .map_err(|error| AgentError::new(format!("{what}: {error}")))?;
        let id = load(harness, places, kinds, (&what, name, ENV_SECRET), config)?;
        loaded.push((format!("secret {name}"), id));
    }
    failed.extend(unstarted(harness, loaded));
    Ok(failed)
}

/// The providers. Returns those that could not be built but that the agent can do without.
fn providers(harness: &Harness, file: &AgentFile) -> Result<Vec<Unstarted>, AgentError> {
    let mut failed = Vec::new();
    for (name, entry) in &file.providers {
        match corduroy_providers::build(name, &entry.spec) {
            Ok(built) => {
                let described = entry
                    .models
                    .iter()
                    .fold(provider(name.clone(), built), |plugin, (model, stated)| {
                        plugin.describing(model.clone(), stated.info())
                    });
                harness.plugin(described);
            }
            // The provider the loop asks is needed. Without another, the add-ons granted its
            // models are told it is unknown when they ask.
            Err(error) if *name == file.provider => {
                return Err(AgentError::new(error.to_string()));
            }
            Err(error) => failed.push(Unstarted {
                plugin: format!("provider {name}"),
                reason: error.to_string(),
            }),
        }
    }
    Ok(failed)
}

/// The extensions, each built from its kind. Returns those that did not start.
fn extend(
    harness: &Harness,
    file: &AgentFile,
    places: &Places,
    standard: &Standard,
) -> Result<Vec<Unstarted>, AgentError> {
    let mut loaded = Vec::new();
    for (name, entry) in &file.extensions {
        let what = format!("extension `{name}`");
        let config = serde_json::to_value(&entry.config)
            .map_err(|error| AgentError::new(format!("{what}: {error}")))?;
        let id = load(
            harness,
            places,
            standard.kinds(),
            (&what, name, &entry.kind),
            config,
        )?;
        loaded.push((format!("extension {name}"), id));
    }
    Ok(unstarted(harness, loaded))
}

/// Those of `loaded` that are not running, with why.
fn unstarted(harness: &Harness, loaded: Vec<(String, FiberId)>) -> Vec<Unstarted> {
    let app = harness.app();
    loaded
        .into_iter()
        .filter_map(|(plugin, id)| {
            let reason = match app.state(id)? {
                FiberState::Failed(reason) => reason.clone(),
                FiberState::Pending => "it is waiting for a service".to_owned(),
                FiberState::Active => return None,
                other => format!("it is {other:?}"),
            };
            Some(Unstarted { plugin, reason })
        })
        .collect()
}

/// The WebAssembly plugins, each with its grants. Returns those that did not start.
fn plugins(
    harness: &Harness,
    file: &AgentFile,
    places: &Places,
    standard: &Standard,
    log: &Path,
) -> Result<Vec<Unstarted>, AgentError> {
    let mut enabled = file.plugins.iter().filter(|entry| entry.enabled).peekable();
    if enabled.peek().is_none() {
        return Ok(Vec::new());
    }
    if !standard.addons() {
        let names: Vec<&str> = enabled.map(|entry| entry.name.as_str()).collect();
        return Err(AgentError::new(format!(
            "this command loads no add-ons, and the agent file lists {}; set `enabled = false` \
             on them or use a command that does",
            names.join(", ")
        )));
    }
    let mut runtime = WasmRuntime::cached(places.data.join("cache"))
        .map_err(|error| AgentError::new(error.to_string()))?
        .with_root(&places.workspace)
        .protect(log);
    if let Some(path) = &places.file {
        runtime = runtime.protect(path);
    }
    let mut loaded = Vec::new();
    for entry in enabled {
        let failed =
            |error: &dyn fmt::Display| AgentError::new(format!("plugin `{}`: {error}", entry.name));
        let component = places.find(&entry.path).map_err(AgentError::new)?;
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
    Ok(unstarted(harness, loaded))
}

#[cfg(test)]
mod tests {
    use corduroy_harness::core::{SECRETS, Secret, Secrets, plugin_fn};

    use super::*;

    #[test]
    fn the_default_agent_parses() {
        let file = AgentFile::parse(&AgentFile::default_text()).unwrap();
        assert_eq!(file.provider, "main");
        assert_eq!(file.plugins.len(), 13);
        assert_eq!(
            file.plugins.iter().filter(|plugin| plugin.enabled).count(),
            12
        );
        assert!(file.plugins[0].grants.contains_key("kernel"));
    }

    /// The model the default agent's `review` asks, and the providers it has.
    fn reviewing(anthropic: bool, openai: bool) -> (String, String, Vec<String>) {
        let file = AgentFile::parse(&default_for(anthropic, openai)).unwrap();
        let review = file.plugins.iter().find(|p| p.name == "review").unwrap();
        let model = review.config.as_ref().unwrap()["model"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(review.grants.contains_key("llm"));
        let providers = file.providers.keys().cloned().collect();
        (file.model, model, providers)
    }

    #[test]
    fn with_both_keys_the_other_vendor_reviews() {
        let (main, review, providers) = reviewing(true, true);
        assert_eq!(main, ANTHROPIC.1);
        assert_eq!(review, format!("reviewer/{}", OPENAI.1));
        assert_eq!(providers, ["main", "reviewer"]);
    }

    #[test]
    fn a_provider_entry_states_what_its_models_hold() {
        let text = "provider = \"p\"\nmodel = \"m\"\n[providers.p]\nkind = \"ollama\"\n\
                    [providers.p.models.m]\ncontext_tokens = 16384\ntools = true\n";
        let file = AgentFile::parse(text).unwrap();
        let stated = file.providers["p"].models["m"].info();
        assert_eq!(stated.context_tokens, Some(16384));
        assert_eq!(stated.tools, Some(true));
        assert_eq!(stated.output_tokens, None);

        // The `llm` grant is the one list of models an add-on may ask, so there is no other.
        let served = "provider = \"p\"\nmodel = \"m\"\n[providers.p]\nkind = \"ollama\"\n\
                      serve = [\"m\"]\n";
        let error = AgentFile::parse(served).unwrap_err().to_string();
        assert!(error.contains("`llm` grant"), "{error}");
        // A key nobody knows is an error, in the provider's entry and in a model's.
        for typo in [
            text.replace("context_tokens", "context"),
            text.replace("kind = \"ollama\"\n", "kind = \"ollama\"\nmax_token = 5\n"),
        ] {
            let error = AgentFile::parse(&typo).unwrap_err().to_string();
            assert!(error.contains("unknown field"), "{error}");
        }
    }

    #[test]
    fn with_one_key_the_same_model_reviews() {
        for (anthropic, openai) in [(true, false), (false, true), (false, false)] {
            let (main, review, providers) = reviewing(anthropic, openai);
            assert_eq!(format!("main/{main}"), review);
            assert_eq!(providers, ["main"]);
        }
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

    /// An agent with no plugins, whose log is in a scratch directory.
    const BARE: &str = "provider = \"p\"\nmodel = \"m\"\n[providers.p]\nkind = \"ollama\"\n";

    #[test]
    fn an_extension_is_built_from_its_kind_and_its_configuration() {
        use corduroy_harness::core::{GATES, Gates};

        let workspace = corduroy_test_support::scratch("cli-extension");
        let places = Places::new(workspace.clone(), None);
        // A kind that declares a decision point named in its configuration.
        let extensions = Extensions::builtin().kind("point", |asked| {
            let point = asked.text("point").ok_or("`point` is required")?.to_owned();
            Ok(Box::new(plugin_fn(asked.name, &[GATES], move |ctx| {
                let gates = ctx.service::<Gates>(GATES)?;
                gates.declare(ctx, point.clone(), "a point of the test's own")?;
                Ok(())
            })))
        });
        let text = format!(
            "{BARE}[extensions.outbound]\nkind = \"point\"\npoint = \"{{workspace}}/leave\"\n\
             [extensions.scratch]\nkind = \"shell-kernel\"\n"
        );
        let file = AgentFile::parse(&text).unwrap();

        let agent = assemble_with(&file, &places, &extensions).unwrap();

        assert_eq!(agent.failed, Vec::new());
        let point = format!("{}/leave", workspace.display());
        assert!(agent.harness.gates().declared(&point));
        assert_eq!(agent.harness.kernels().names(), ["scratch"]);
    }

    #[test]
    fn an_extension_of_no_known_kind_or_with_a_bad_entry_is_named() {
        let places = Places::new(corduroy_test_support::scratch("cli-extension-bad"), None);
        let unknown = format!("{BARE}[extensions.box]\nkind = \"docker-kernel\"\n");
        let file = AgentFile::parse(&unknown).unwrap();
        let error = assemble(&file, &places).unwrap_err().to_string();
        assert_eq!(
            error,
            "extension `box`: this command has no extension kind `docker-kernel` (it has: \
             env-secret, http-client, mcp, powershell-kernel, shell-kernel)"
        );

        let refusing = Extensions::none().kind("picky", |_| Err("`image` is required".into()));
        let picky = format!("{BARE}[extensions.box]\nkind = \"picky\"\n");
        let file = AgentFile::parse(&picky).unwrap();
        let error = assemble_with(&file, &places, &refusing)
            .unwrap_err()
            .to_string();
        assert_eq!(error, "extension `box`: `image` is required");
    }

    #[test]
    fn a_kernel_is_built_from_its_kind_and_refuses_what_it_does_not_know() {
        let places = Places::new(corduroy_test_support::scratch("cli-kernel-kinds"), None);
        let bash = format!("{BARE}[kernels.work]\nkind = \"shell\"\nprogram = \"bash\"\n");
        let agent = assemble(&AgentFile::parse(&bash).unwrap(), &places).unwrap();
        assert_eq!(agent.harness.kernels().names(), ["work"]);
        // The log stays locked while an agent holds it.
        drop(agent);

        let typo = format!("{BARE}[kernels.work]\nkind = \"shell\"\ndri = \"/tmp\"\n");
        let error = assemble(&AgentFile::parse(&typo).unwrap(), &places)
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "kernel `work`: `dri` is not a setting of this kind (it has: dir, program)"
        );

        let docker = format!("{BARE}[kernels.box]\nkind = \"docker\"\n");
        let file = AgentFile::parse(&docker).unwrap();
        let error = assemble(&file, &places).unwrap_err().to_string();
        assert_eq!(
            error,
            "kernel `box` is of kind `docker`, which this command does not have (it has: \
             powershell, shell)"
        );
        // A command that brings the kind has it.
        let with_docker =
            Standard::new().with_kinds(&Extensions::none().kind(kernel_kind("docker"), |asked| {
                Ok(Box::new(corduroy_std::shell::shell_kernel(asked.name, "/")))
            }));
        let agent = assemble_from(&file, &places, &with_docker).unwrap();
        assert_eq!(agent.harness.kernels().names(), ["box"]);
    }

    #[test]
    fn the_standard_parts_can_be_swapped() {
        use corduroy_harness::core::EventStream;

        let workspace = corduroy_test_support::scratch("cli-standard-parts");
        let places = Places::new(workspace, None);
        let file = AgentFile::parse(BARE).unwrap();

        // The HTTP client comes with every agent, unless the command leaves it out.
        let agent = assemble(&file, &places).unwrap();
        assert_eq!(agent.harness.http().names(), ["http", "https"]);
        drop(agent);
        let bare = Standard::new().without_kind("http-client");
        let agent = assemble_from(&file, &places, &bare).unwrap();
        assert_eq!(agent.harness.http().names(), Vec::<String>::new());
        drop(agent);

        // Another store for the log: here, none at all.
        let elsewhere = Places::new(corduroy_test_support::scratch("cli-standard-log"), None);
        let in_memory = Standard::new().with_log(|_| Ok(EventStream::new()));
        let agent = assemble_from(&file, &elsewhere, &in_memory).unwrap();
        assert!(!agent.log.exists());

        // A command without add-ons refuses an agent that lists one.
        let listed = format!("{BARE}[[plugins]]\nname = \"a\"\npath = \"a.wasm\"\n");
        let error = assemble_from(
            &AgentFile::parse(&listed).unwrap(),
            &places,
            &Standard::new().without_addons(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("loads no add-ons"), "{error}");
    }

    #[test]
    fn secrets_and_mcp_servers_are_built_from_kinds_a_command_can_swap() {
        let places = Places::new(corduroy_test_support::scratch("cli-secret-kinds"), None);
        let file = AgentFile::parse(&format!(
            "{BARE}[secrets.token]\nenv = \"CORDUROY_TEST_TOKEN\"\n\
             [mcp.remote]\nurl = \"http://127.0.0.1:9/mcp\"\nbearer_token_env = \"CORDUROY_UNSET\"\n"
        ))
        .unwrap();

        // The standard kinds: the secret from the environment; the MCP server's bearer token
        // is read when it is built, and its variable is not set.
        let error = assemble(&file, &places).unwrap_err().to_string();
        assert_eq!(error, "MCP server `remote`: $CORDUROY_UNSET is not set");

        // A command of its own keeps secrets elsewhere and reaches MCP servers its own way.
        let own = Extensions::none()
            .kind(ENV_SECRET, |asked| {
                let name = format!("{}-from-the-keychain", asked.name);
                Ok(Box::new(plugin_fn(asked.name, &[SECRETS], move |ctx| {
                    let secret: Arc<dyn Secret> = Arc::new(|| Ok("value".to_owned()));
                    ctx.service::<Secrets>(SECRETS)?
                        .register(ctx, name.clone(), secret)?;
                    Ok(())
                })))
            })
            .kind(MCP, |asked| {
                Ok(Box::new(plugin_fn(asked.name, &[], |_| Ok(()))))
            });
        let agent = assemble_with(&file, &places, &own).unwrap();
        assert_eq!(agent.harness.secrets().names(), ["token-from-the-keychain"]);
        assert!(agent.failed.is_empty(), "{:?}", agent.failed);
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
