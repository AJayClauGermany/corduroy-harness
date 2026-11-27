//! Model providers for the corduroy harness.
//!
//! The harness asks whatever provider is registered under a name; this crate makes real ones,
//! from [`oi-kit-providers`](oi_kit_providers), the provider crate Goose is built on. A
//! [`ProviderSpec`] says which: one of the engines (`anthropic`, `openai`, `ollama`, and their
//! `-compatible` forms for other endpoints speaking the same protocol), or the name of one of
//! the providers oi-kit already knows (`groq`, `mistral`, `deepseek` and the rest; see
//! [`known`]).
//!
//! A provider's credential is the secret the spec names (`secret`), revealed from the
//! [`Secrets`] registry for every request, so a credential that changes while the agent runs
//! (a token that is renewed) is the one sent. Without `secret`, an API key is read from the
//! environment variable the spec names, by the provider, when it is built. Either way it is
//! never part of the spec, the agent file or the event log.
//!
//! Each request asks for at most the spec's `max_tokens`, by default the most the model may
//! write as the spec's `models` or oi-kit's model catalog have it (for a model neither
//! describes, a quarter of the context `models` states, or 4096), and thinks only as hard
//! as the spec's `thinking` says. One attempt at a request has `timeout_seconds` in all,
//! streamed or not, and one that runs out ends the request as a timeout. A request to `OpenAI`'s Responses API is the
//! exception: it carries a limit only when the spec states one, and the system prompt as
//! `instructions`, which is the shape an account paying through a `ChatGPT` plan is held to.
//!
//! What is built is a [`corduroy_harness::Provider`]: it speaks the harness's own messages,
//! and converts to and from oi-kit's for each request. oi-kit's types go no further than this
//! crate, so the harness, its log and its plugins do not depend on them.
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

pub mod adapter;
pub mod auth;

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use corduroy_harness::core::Secrets;
use corduroy_harness::{ModelInfo, Provider};
use oi_kit_provider_types::base::Provider as OiKitProvider;
use oi_kit_providers::declarative::{
    EnvKeyResolver, deserialize_provider_config, fixed_provider_config_entries, from_json,
};
use oi_kit_providers::{anthropic, openai};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// Which provider to build, and how to reach it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
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
    /// The secret holding the credential: the name of an entry in the [`Secrets`] registry,
    /// revealed for every request. In place of `api_key_env`, for the engines that take a
    /// key (`anthropic`, `openai` and their `-compatible` forms).
    #[serde(default)]
    pub secret: Option<String>,
    /// Extra headers sent with every request.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Seconds one attempt at a request may take in all, from sending it to the end of its
    /// answer, streamed or not: 600 by default. An attempt that runs out is not tried
    /// again; one retried for another reason (a rate limit, a dropped connection) starts
    /// the count afresh.
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    /// Whether to stream responses. Each engine has a default.
    #[serde(default)]
    pub streaming: Option<bool>,
    /// The most tokens one answer may take. By default, the most the model may write as
    /// [`models`](Self::models) states it, or as oi-kit's model catalog has it; for a model
    /// neither describes, a quarter of the context `models` states (at least 1024), or 4096.
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Sampling temperature. By default, the model's own.
    #[serde(default)]
    pub temperature: Option<f32>,
    /// How hard the model thinks before it answers, for a model that can. By default it does
    /// not, unless the model always does.
    #[serde(default)]
    pub thinking: Option<Thinking>,
    /// What the host states about models of this provider, by the provider's name for
    /// each: the default for `max_tokens` is taken from it. Not a key of the entry this is
    /// read from; whoever reads the entry fills it in.
    #[serde(skip)]
    pub models: BTreeMap<String, ModelInfo>,
}

/// Seconds one attempt at a request may take where the spec does not say.
const DEFAULT_TIMEOUT_SECONDS: u64 = 600;

/// How hard a model thinks before it answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Thinking {
    /// Not at all.
    Off,
    /// A little.
    Low,
    /// Some.
    Medium,
    /// A lot.
    High,
    /// As much as the model can.
    Max,
}

impl ProviderSpec {
    /// A spec for `kind` with every default.
    #[must_use]
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            base_url: None,
            api_key_env: None,
            secret: None,
            headers: BTreeMap::new(),
            timeout_seconds: None,
            streaming: None,
            max_tokens: None,
            temperature: None,
            thinking: None,
            models: BTreeMap::new(),
        }
    }

    /// The environment variable this provider's key is read from: the one it names
    /// (`api_key_env`), or its kind's own. `None` where it names a `secret`, needs no key,
    /// or is of a kind beyond the engines, whose variables are oi-kit's to know.
    #[must_use]
    pub fn key_variable(&self) -> Option<String> {
        if self.secret.is_some() {
            return None;
        }
        self.api_key_env
            .clone()
            .or_else(|| Some(engine(&self.kind)?.api_key_env.to_owned()))
            .filter(|variable| !variable.is_empty())
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

/// An engine: the protocol, what oi-kit calls a provider of it, where it is by default, and
/// the key it needs by default.
struct Engine {
    protocol: &'static str,
    id: &'static str,
    base_url: Option<&'static str>,
    api_key_env: &'static str,
}

fn engine(kind: &str) -> Option<Engine> {
    // A `-compatible` endpoint is no provider oi-kit knows: its id matches no entry of the
    // model catalog and none of the special cases oi-kit keeps for providers by name.
    let (protocol, id, base_url, api_key_env) = match kind {
        "anthropic" => (
            "anthropic",
            "anthropic",
            Some("https://api.anthropic.com"),
            "ANTHROPIC_API_KEY",
        ),
        "openai" => (
            "openai",
            "openai",
            Some("https://api.openai.com/v1/chat/completions"),
            "OPENAI_API_KEY",
        ),
        "ollama" => ("ollama", "ollama", Some("http://localhost:11434"), ""),
        "anthropic-compatible" => ("anthropic", "anthropic_compatible", None, ""),
        "openai-compatible" => ("openai", "openai_compatible", None, ""),
        _ => return None,
    };
    Some(Engine {
        protocol,
        id,
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
        // A secret stands in for the key: nothing is read from the environment then.
        let api_key_env = if spec.secret.is_some() {
            ""
        } else {
            spec.api_key_env.as_deref().unwrap_or(engine.api_key_env)
        };
        Map::from_iter([
            ("name".to_owned(), json!(engine.id)),
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
        config.insert("display_name".to_owned(), json!(name));
        config
    };
    // `name` stays oi-kit's own id for the provider (`groq`, `anthropic`): it looks up the
    // model catalog and its special cases by it. Ours is the display name.
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
/// Returns a [`BuildError`] for an unknown kind, a missing base URL or API key, a spec that
/// names a `secret` (which needs [`build_with`]), or a configuration oi-kit refuses.
pub fn build(name: &str, spec: &ProviderSpec) -> Result<Arc<dyn Provider>, BuildError> {
    assemble(name, spec, None)
}

/// Build the provider `spec` describes, with the secret it names taken from `secrets`.
///
/// The secret is revealed each time the provider sends a request. It need not be registered
/// yet: a request made while it is missing fails, saying so.
///
/// # Errors
///
/// As [`build`], and for a spec whose kind takes no `secret` or that names both a `secret`
/// and an `api_key_env`.
pub fn build_with(
    name: &str,
    spec: &ProviderSpec,
    secrets: &Arc<Secrets>,
) -> Result<Arc<dyn Provider>, BuildError> {
    assemble(name, spec, Some(secrets))
}

/// The path of `url`: what follows its host, without the query.
fn path_of(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = after_scheme
        .find('/')
        .map_or("", |at| after_scheme.split_at(at).1);
    path.split(['?', '#']).next().unwrap_or(path)
}

/// A secret a provider sends as its credential: the registry it is in, and its name there.
type Named = (Arc<Secrets>, String);

/// The secret `spec` names, if it names one.
fn credential(
    name: &str,
    spec: &ProviderSpec,
    secrets: Option<&Arc<Secrets>>,
) -> Result<Option<Named>, BuildError> {
    match (&spec.secret, secrets) {
        (None, _) => Ok(None),
        (Some(_), _) if spec.api_key_env.is_some() => Err(BuildError(format!(
            "provider `{name}` names both a secret and an api_key_env: give one"
        ))),
        (Some(secret), Some(secrets)) => Ok(Some((Arc::clone(secrets), secret.clone()))),
        (Some(secret), None) => Err(BuildError(format!(
            "provider `{name}` names the secret `{secret}`, and was built without the secrets"
        ))),
    }
}

fn assemble(
    name: &str,
    spec: &ProviderSpec,
    secrets: Option<&Arc<Secrets>>,
) -> Result<Arc<dyn Provider>, BuildError> {
    let config = config(name, spec)?;
    if let Some(temperature) = spec.temperature
        && !(0.0..=2.0).contains(&temperature)
    {
        return Err(BuildError(format!(
            "provider `{name}`: temperature {temperature} is not between 0 and 2"
        )));
    }
    let refused = |error: anyhow::Error| BuildError(format!("provider `{name}`: {error:#}"));
    let secret = credential(name, spec, secrets)?;
    let protocol = engine(&spec.kind).map(|engine| engine.protocol);
    let provider: Arc<dyn OiKitProvider> = match protocol {
        Some("openai") => {
            let parsed = deserialize_provider_config(&config.to_string()).map_err(refused)?;
            let builder = openai::from_declarative_config(parsed, None, EnvKeyResolver::new())
                .map_err(refused)?
                .map_api_client(|client| match secret {
                    Some((secrets, secret)) => client.with_auth(auth::bearer(secrets, secret)),
                    None => client,
                });
            Arc::new(builder.build())
        }
        Some("anthropic") if secret.is_some() => {
            let parsed = deserialize_provider_config(&config.to_string()).map_err(refused)?;
            let builder = anthropic::from_declarative_config(parsed, None, EnvKeyResolver::new())
                .map_err(refused)?
                .map_api_client(|client| match secret {
                    Some((secrets, secret)) => client.with_auth(auth::api_key(secrets, secret)),
                    None => client,
                });
            Arc::new(builder.build())
        }
        _ if secret.is_some() => {
            return Err(BuildError(format!(
                "provider `{name}` ({}) takes no secret: `secret` is for anthropic, openai \
                 and their -compatible forms",
                spec.kind
            )));
        }
        _ => {
            Arc::from(from_json(&config.to_string(), None, EnvKeyResolver::new()).map_err(refused)?)
        }
    };
    let settings = adapter::Settings {
        provider: provider.get_name().to_owned(),
        openai_path: (protocol == Some("openai"))
            .then(|| config.get("base_url").and_then(Value::as_str))
            .flatten()
            .map(|url| openai::derive_base_path(path_of(url))),
        max_tokens: spec.max_tokens,
        temperature: spec.temperature,
        thinking: spec.thinking,
        timeout: Duration::from_secs(spec.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECONDS)),
        stated: spec.models.clone(),
    };
    Ok(Arc::new(adapter::Adapted::new(provider, settings)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engines_fill_in_their_defaults() {
        let anthropic = config("main", &ProviderSpec::new("anthropic")).unwrap();
        assert_eq!(anthropic["engine"], "anthropic");
        assert_eq!(anthropic["name"], "anthropic");
        assert_eq!(anthropic["display_name"], "main");
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
        let compatible = config("x", &spec).unwrap();
        assert_eq!(compatible["engine"], "openai");
        assert_eq!(compatible["name"], "openai_compatible");
    }

    #[test]
    fn settings_are_read_from_the_agent_file() {
        let spec: ProviderSpec = toml::from_str(
            r#"
            kind = "anthropic"
            max_tokens = 32000
            temperature = 0.5
            thinking = "medium"
            "#,
        )
        .unwrap();
        assert_eq!(spec.max_tokens, Some(32000));
        assert_eq!(spec.temperature, Some(0.5));
        assert_eq!(spec.thinking, Some(Thinking::Medium));
        assert!(
            toml::from_str::<ProviderSpec>("kind = \"anthropic\"\nthinking = \"lots\"").is_err()
        );
    }

    #[test]
    fn a_temperature_out_of_range_is_refused() {
        let spec = ProviderSpec {
            temperature: Some(3.0),
            ..ProviderSpec::new("ollama")
        };
        let error = build("local", &spec).err().unwrap();
        assert!(error.to_string().contains("temperature 3"), "{error}");
    }

    #[test]
    fn known_providers_are_found_by_name_and_unknown_ones_are_listed() {
        assert!(known().contains(&"groq".to_owned()));
        let groq = config("fast", &ProviderSpec::new("groq")).unwrap();
        // oi-kit knows it as `groq` (and treats it as such); we call it `fast`.
        assert_eq!(groq["name"], "groq");
        assert_eq!(groq["display_name"], "fast");
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
    fn a_secret_stands_in_for_the_key_and_nothing_is_read_from_the_environment() {
        let spec = ProviderSpec {
            secret: Some("key".into()),
            ..ProviderSpec::new("openai")
        };
        let named = config("main", &spec).unwrap();
        assert_eq!(named["api_key_env"], "");
        assert_eq!(named["requires_auth"], false);
        // No secret is registered and no key is set: it builds, and would fail when asked.
        let secrets = Arc::new(Secrets::new());
        assert!(build_with("main", &spec, &secrets).is_ok());
        let anthropic = ProviderSpec {
            secret: Some("key".into()),
            ..ProviderSpec::new("anthropic")
        };
        assert!(build_with("main", &anthropic, &secrets).is_ok());
    }

    #[test]
    fn a_secret_is_refused_where_it_cannot_be_used() {
        let secrets = Arc::new(Secrets::new());
        let named = |kind: &str| ProviderSpec {
            secret: Some("key".into()),
            ..ProviderSpec::new(kind)
        };
        let error = build("main", &named("openai")).err().unwrap();
        assert!(error.to_string().contains("without the secrets"), "{error}");
        let error = build_with("local", &named("ollama"), &secrets)
            .err()
            .unwrap();
        assert!(error.to_string().contains("takes no secret"), "{error}");
        let both = ProviderSpec {
            api_key_env: Some("SOME_KEY".into()),
            ..named("openai")
        };
        let error = build_with("main", &both, &secrets).err().unwrap();
        assert!(error.to_string().contains("give one"), "{error}");
    }

    #[test]
    fn a_urls_path_is_what_follows_its_host() {
        assert_eq!(
            path_of("https://api.openai.com/v1/chat/completions"),
            "/v1/chat/completions"
        );
        assert_eq!(
            path_of("http://127.0.0.1:8080/v1/responses?x=1"),
            "/v1/responses"
        );
        assert_eq!(path_of("https://example.com"), "");
        assert_eq!(path_of("localhost:11434/api"), "/api");
    }

    #[test]
    fn a_provider_needing_no_key_builds_without_the_network() {
        assert!(build("local", &ProviderSpec::new("ollama")).is_ok());
    }
}
