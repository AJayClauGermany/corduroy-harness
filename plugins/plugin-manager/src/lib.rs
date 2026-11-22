//! Find, load and unload WebAssembly plugins while the agent runs, after Goose's Extension
//! Manager.
//!
//! The model can see which components are on hand, load one with a configuration and grants,
//! and unload what it loaded. Everything a load asks for is checked by the host: the path
//! must be one this plugin can read, the name one its `plugins` scope reaches, each grant is
//! narrowed to this plugin's own (a plugin never grants more than it holds), and the load is
//! asked of the gates at `plugin/load`. A loaded plugin starts before the model's next step,
//! and goes when this plugin does.
//!
//! Configuration: `dirs`, where to look for components. Needs `fs-read` on them and
//! `plugins`; to hand a loaded plugin a capability it must hold that capability itself.
//!
//! Tools: `plugin_search`, `plugin_list`, `plugin_load`, `plugin_unload`.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use corduroy_guest::host::fs_read;
use corduroy_guest::host::plugins::{self, Grant, LoadRequest};
use corduroy_guest::{Plugin, PluginInfo, ToolError, ToolSpec, parse};
use serde::Deserialize;

#[derive(Deserialize, Default)]
#[serde(default)]
struct Config {
    dirs: Vec<String>,
}

static CONFIG: OnceLock<Config> = OnceLock::new();

/// A grant as the model writes it: patterns, or allow and deny lists.
#[derive(Deserialize)]
#[serde(untagged)]
enum Scope {
    One(String),
    Many(Vec<String>),
    Scoped {
        allow: Vec<String>,
        #[serde(default)]
        deny: Vec<String>,
    },
}

#[derive(Deserialize)]
struct Load {
    name: String,
    path: String,
    #[serde(default)]
    config: Option<serde_json::Value>,
    #[serde(default)]
    grants: BTreeMap<String, Scope>,
}

#[derive(Deserialize)]
struct Unload {
    name: String,
}

fn search() -> String {
    let dirs = &CONFIG.get_or_init(Config::default).dirs;
    let mut found = Vec::new();
    for dir in dirs {
        // A directory that is missing or out of reach has nothing to offer.
        let Ok(entries) = fs_read::list(dir) else {
            continue;
        };
        found.extend(
            entries
                .iter()
                .filter(|name| {
                    std::path::Path::new(name.as_str())
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("wasm"))
                })
                .map(|name| format!("{}/{name}", dir.trim_end_matches('/'))),
        );
    }
    if found.is_empty() {
        "No components were found.".to_owned()
    } else {
        found.join("\n")
    }
}

fn load(input: Load) -> Result<String, ToolError> {
    let grants = input
        .grants
        .into_iter()
        .map(|(capability, scope)| {
            let (allow, deny) = match scope {
                Scope::One(pattern) => (vec![pattern], Vec::new()),
                Scope::Many(patterns) => (patterns, Vec::new()),
                Scope::Scoped { allow, deny } => (allow, deny),
            };
            Grant {
                capability,
                allow,
                deny,
            }
        })
        .collect();
    let request = LoadRequest {
        name: input.name.clone(),
        path: input.path,
        config: input
            .config
            .map_or_else(|| "{}".to_owned(), |config| config.to_string()),
        grants,
    };
    let hash = plugins::load(&request)?;
    Ok(format!(
        "Loading `{}` (sha256 {hash}). Its tools are offered from your next step.",
        input.name
    ))
}

struct Manager;

impl Plugin for Manager {
    fn activate(config: String) -> Result<PluginInfo, String> {
        let config: Config = corduroy_guest::config(&config)?;
        CONFIG.get_or_init(|| config);
        Ok(PluginInfo::new("plugin-manager", env!("CARGO_PKG_VERSION"))
            .tool(
                ToolSpec::new(
                    "plugin_search",
                    "List the plugin components that can be loaded.",
                    r#"{"type":"object","properties":{}}"#,
                )
                .read_only()
                .replay_safe(),
            )
            .tool(
                ToolSpec::new(
                    "plugin_list",
                    "List the plugins you loaded.",
                    r#"{"type":"object","properties":{}}"#,
                )
                .read_only()
                .replay_safe(),
            )
            .tool(ToolSpec::new(
                "plugin_load",
                "Load a plugin component under a name, with its configuration and the \
                 capabilities it is granted (each a pattern, a list of patterns, or \
                 {allow, deny}). It can be granted no more than this plugin holds.",
                r#"{"type":"object","properties":{"name":{"type":"string"},"path":{"type":"string"},"config":{"type":"object"},"grants":{"type":"object"}},"required":["name","path"]}"#,
            ))
            .tool(ToolSpec::new(
                "plugin_unload",
                "Unload a plugin you loaded.",
                r#"{"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}"#,
            )))
    }

    fn call_tool(name: String, input: String) -> Result<String, ToolError> {
        match name.as_str() {
            "plugin_search" => Ok(search()),
            "plugin_list" => {
                let loaded = plugins::loaded();
                Ok(if loaded.is_empty() {
                    "None.".to_owned()
                } else {
                    loaded.join("\n")
                })
            }
            "plugin_load" => load(parse(&input)?),
            "plugin_unload" => {
                let Unload { name } = parse(&input)?;
                plugins::unload(&name)?;
                Ok(format!("Unloaded `{name}`."))
            }
            _ => Err(ToolError::UnknownTool(name)),
        }
    }
}

corduroy_guest::export!(Manager);
