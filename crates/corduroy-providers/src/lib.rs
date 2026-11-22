//! Model providers for the corduroy harness.
//!
//! The harness asks whatever provider is registered under a name; this crate makes real ones,
//! from [`oi-kit-providers`](oi_kit_providers), the provider crate Goose is built on. A
//! [`ProviderSpec`] says which: one of the engines (`anthropic`, `openai`, `ollama`, and their
//! `-compatible` forms for other endpoints speaking the same protocol), or the name of one of
//! the providers oi-kit already knows (`groq`, `mistral`, `deepseek` and the rest; see
//! [`known`]).
//!
//! API keys are read from the environment variable the spec names, by the provider, when it
//! is built. They are never part of the spec, the agent file or the event log.
//!
//! ```no_run
//! use corduroy_providers::{ProviderSpec, build};
//!
//! let spec = ProviderSpec::new("anthropic");
//! let provider = build("anthropic", &spec)?;   // reads ANTHROPIC_API_KEY
//! # Ok::<(), corduroy_providers::BuildError>(())
//! ```

#![expect(
    clippy::multiple_crate_versions,
    reason = "the oi-kit and reqwest dependency trees pull several versions of shared crates"
)]

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use oi_kit_provider_types::base::Provider;
use oi_kit_providers::declarative::{EnvKeyResolver, fixed_provider_config_entries, from_json};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// Which provider to build, and how to reach it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSpec {
    /// `anthropic`, `openai`, `ollama`, `anthropic-compatible`, `openai-compatible`, or the
    /// name of a provider oi-kit knows (see [`known`]).
    pub kind: String,
    /// Where the API is. Each engine has a default; a `-compatible` kind needs one.
    #[serde(default)]
    pub base_url: Option<String>,
    /// The environment variable holding the API key. Each engine has a default
    /// (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`); Ollama needs none.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Extra headers sent with every request.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Seconds a request may take in all.
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    /// Whether to stream responses. Each engine has a default.
    #[serde(default)]
    pub streaming: Option<bool>,
}

impl ProviderSpec {
    /// A spec for `kind` with every default.
    #[must_use]
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            base_url: None,
            api_key_env: None,
            headers: BTreeMap::new(),
            timeout_seconds: None,
            streaming: None,
        }
    }
}

/// Why a provider could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildError(String);

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BuildError {}

/// An engine: the protocol, where it is by default, and the key it needs by default.
struct Engine {
    protocol: &'static str,
    base_url: Option<&'static str>,
    api_key_env: &'static str,
}

fn engine(kind: &str) -> Option<Engine> {
    let (protocol, base_url, api_key_env) = match kind {
        "anthropic" => (
            "anthropic",
            Some("https://api.anthropic.com"),
            "ANTHROPIC_API_KEY",
        ),
        "openai" => (
            "openai",
            Some("https://api.openai.com/v1/chat/completions"),
            "OPENAI_API_KEY",
        ),
        "ollama" => ("ollama", Some("http://localhost:11434"), ""),
        "anthropic-compatible" => ("anthropic", None, ""),
        "openai-compatible" => ("openai", None, ""),
        _ => return None,
    };
    Some(Engine {
        protocol,
        base_url,
        api_key_env,
    })
}

/// The providers oi-kit knows by name, beyond the engines, sorted.
#[must_use]
pub fn known() -> Vec<String> {
    let mut names: Vec<String> = fixed_provider_config_entries()
        .into_iter()
        .filter_map(|(file, _)| file.strip_suffix(".json").map(str::to_owned))
        .collect();
    names.sort();
    names
}

/// The declarative configuration oi-kit builds a provider from.
fn config(name: &str, spec: &ProviderSpec) -> Result<Value, BuildError> {
    let mut config: Map<String, Value> = if let Some(engine) = engine(&spec.kind) {
        let base_url = spec
            .base_url
            .as_deref()
            .or(engine.base_url)
            .ok_or_else(|| {
                BuildError(format!(
                    "provider `{name}` ({}) needs a base_url",
                    spec.kind
                ))
            })?;
        let api_key_env = spec.api_key_env.as_deref().unwrap_or(engine.api_key_env);
        Map::from_iter([
            ("engine".to_owned(), json!(engine.protocol)),
            ("display_name".to_owned(), json!(name)),
            ("base_url".to_owned(), json!(base_url)),
            ("api_key_env".to_owned(), json!(api_key_env)),
            ("requires_auth".to_owned(), json!(!api_key_env.is_empty())),
            ("models".to_owned(), json!([])),
        ])
    } else {
        let file = format!("{}.json", spec.kind);
        let (_, text) = fixed_provider_config_entries()
            .into_iter()
            .find(|(entry, _)| *entry == file)
            .ok_or_else(|| {
                BuildError(format!(
                    "unknown provider kind `{}`: use anthropic, openai, ollama, \
                     anthropic-compatible, openai-compatible or one of: {}",
                    spec.kind,
                    known().join(", ")
                ))
            })?;
        let mut config: Map<String, Value> = serde_json::from_str(text).map_err(|error| {
            BuildError(format!("oi-kit's definition of `{}`: {error}", spec.kind))
        })?;
        if let Some(base_url) = &spec.base_url {
            config.insert("base_url".to_owned(), json!(base_url));
        }
        if let Some(api_key_env) = &spec.api_key_env {
            config.insert("api_key_env".to_owned(), json!(api_key_env));
        }
        config
    };
    config.insert("name".to_owned(), json!(name));
    if !spec.headers.is_empty() {
        config.insert("headers".to_owned(), json!(spec.headers));
    }
    if let Some(timeout) = spec.timeout_seconds {
        config.insert("timeout_seconds".to_owned(), json!(timeout));
    }
    if let Some(streaming) = spec.streaming {
        config.insert("supports_streaming".to_owned(), json!(streaming));
    }
    Ok(Value::Object(config))
}

/// Build the provider `spec` describes, named `name`. Its API key, if it needs one, is read
/// from the environment now.
///
/// # Errors
///
/// Returns a [`BuildError`] for an unknown kind, a missing base URL or API key, or a
/// configuration oi-kit refuses.
pub fn build(name: &str, spec: &ProviderSpec) -> Result<Arc<dyn Provider>, BuildError> {
    let config = config(name, spec)?;
    let provider = from_json(&config.to_string(), None, EnvKeyResolver::new())
        .map_err(|error| BuildError(format!("provider `{name}`: {error:#}")))?;
    Ok(Arc::from(provider))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engines_fill_in_their_defaults() {
        let anthropic = config("main", &ProviderSpec::new("anthropic")).unwrap();
        assert_eq!(anthropic["engine"], "anthropic");
        assert_eq!(anthropic["base_url"], "https://api.anthropic.com");
        assert_eq!(anthropic["api_key_env"], "ANTHROPIC_API_KEY");
        assert_eq!(anthropic["requires_auth"], true);

        let ollama = config("local", &ProviderSpec::new("ollama")).unwrap();
        assert_eq!(ollama["requires_auth"], false);
    }

    #[test]
    fn a_compatible_endpoint_needs_its_url() {
        let error = config("x", &ProviderSpec::new("openai-compatible")).unwrap_err();
        assert!(error.to_string().contains("needs a base_url"));
        let spec = ProviderSpec {
            base_url: Some("http://127.0.0.1:9/v1/chat/completions".into()),
            ..ProviderSpec::new("openai-compatible")
        };
        assert_eq!(config("x", &spec).unwrap()["engine"], "openai");
    }

    #[test]
    fn known_providers_are_found_by_name_and_unknown_ones_are_listed() {
        assert!(known().contains(&"groq".to_owned()));
        let groq = config("fast", &ProviderSpec::new("groq")).unwrap();
        assert_eq!(groq["name"], "fast");
        assert_eq!(groq["api_key_env"], "GROQ_API_KEY");
        let error = config("x", &ProviderSpec::new("nonesuch")).unwrap_err();
        assert!(error.to_string().contains("groq"));
    }

    #[test]
    fn a_missing_key_is_reported_by_name() {
        let spec = ProviderSpec {
            api_key_env: Some("CORDUROY_TEST_KEY_THAT_IS_NOT_SET".into()),
            ..ProviderSpec::new("anthropic")
        };
        let error = build("main", &spec).err().unwrap();
        assert!(
            error
                .to_string()
                .contains("CORDUROY_TEST_KEY_THAT_IS_NOT_SET"),
            "{error}"
        );
    }

    #[test]
    fn a_provider_needing_no_key_builds_without_the_network() {
        let provider = build("local", &ProviderSpec::new("ollama")).unwrap();
        assert_eq!(provider.get_name(), "local");
    }
}
