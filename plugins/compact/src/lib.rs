//! Auto-compaction, after Goose: the start of a long conversation summarised by a model.
//!
//! The harness asks the service this plugin provides, `compaction`, before each step of the
//! loop: `due` says whether the conversation should be condensed, and `summarize` writes the
//! summary the model will see in place of the messages before the current turn. The harness
//! picks where to cut and records the summary; the messages stay in the log.
//!
//! Configuration: `model`, the model that writes summaries, as `<provider>/<model>` (the
//! `llm` grant must reach it); `max_tokens`, the input tokens of a request past which the
//! conversation is due; and `max_messages`, a count of earlier messages past which it is due
//! whatever the tokens (off by default). Without `max_tokens` the conversation is due at
//! three quarters of what the session's model holds, which the harness passes on when the
//! model's description says, and at 100 000 tokens when it does not.

use std::sync::OnceLock;

use corduroy_guest::host::llm::{self, Completion, Role, Turn};
use corduroy_guest::{CallError, Plugin, PluginInfo, ServiceCall};
use serde::Deserialize;
use serde_json::json;

const SERVICE: &str = "compaction";

const INSTRUCTIONS: &str = "You condense the earlier part of a conversation between a user and \
an AI agent that uses tools, so the agent can carry on without it. Keep what the agent needs: \
the user's goals and preferences, decisions made and why, files and commands that matter with \
their exact names, what was tried and failed, and what is left to do. Leave out pleasantries \
and tool output that no longer matters. Write it as notes to the agent, in the second person.";

#[derive(Deserialize)]
struct Config {
    model: String,
    #[serde(default)]
    max_tokens: Option<u64>,
    #[serde(default)]
    max_messages: Option<u64>,
}

/// The input tokens past which a conversation is due when nothing says how much its model
/// holds.
const DEFAULT_MAX_TOKENS: u64 = 100_000;

impl Config {
    /// The input tokens past which a conversation is due, for a model that holds `context`
    /// tokens if that is known: what was configured, else three quarters of the context,
    /// which leaves room for the turn in progress and the answer.
    fn limit(&self, context: Option<u64>) -> u64 {
        self.max_tokens
            .or_else(|| context.map(|context| context / 4 * 3))
            .unwrap_or(DEFAULT_MAX_TOKENS)
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();

#[derive(Deserialize)]
struct Due {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    messages: u64,
    /// How much the session's model holds, if its description says.
    #[serde(default)]
    context_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct Summarize {
    transcript: String,
}

fn config() -> Result<&'static Config, CallError> {
    CONFIG
        .get()
        .ok_or_else(|| CallError::Failed("compact was not configured".to_owned()))
}

fn parse<T: for<'a> Deserialize<'a>>(payload: &str) -> Result<T, CallError> {
    serde_json::from_str(payload).map_err(|error| CallError::InvalidInput(error.to_string()))
}

struct Compact;

impl Plugin for Compact {
    fn activate(config: String) -> Result<PluginInfo, String> {
        let config: Config = corduroy_guest::config(&config)?;
        CONFIG.get_or_init(|| config);
        Ok(PluginInfo::new("compact", env!("CARGO_PKG_VERSION")).provide(SERVICE))
    }

    fn handle_call(call: ServiceCall) -> Result<String, CallError> {
        if call.service != SERVICE {
            return Err(CallError::Unknown(format!("service `{}`", call.service)));
        }
        let config = config()?;
        match call.method.as_str() {
            "due" => {
                let Due {
                    input_tokens,
                    messages,
                    context_tokens,
                } = parse(&call.payload)?;
                let due = input_tokens >= config.limit(context_tokens)
                    || config.max_messages.is_some_and(|most| messages >= most);
                Ok(json!({ "due": due }).to_string())
            }
            "summarize" => {
                let Summarize { transcript } = parse(&call.payload)?;
                let request = Completion {
                    system: INSTRUCTIONS.to_owned(),
                    messages: vec![Turn {
                        role: Role::User,
                        text: format!("Condense this conversation:\n\n{transcript}"),
                    }],
                };
                let summary = llm::complete(&config.model, &request)
                    .map_err(|error| CallError::Failed(format!("{error:?}")))?;
                Ok(json!({ "summary": summary }).to_string())
            }
            method => Err(CallError::Unknown(format!("method `{method}`"))),
        }
    }
}

corduroy_guest::export!(Compact);

#[cfg(test)]
mod tests {
    use super::*;

    fn config(max_tokens: Option<u64>) -> Config {
        Config {
            model: "main/small".to_owned(),
            max_tokens,
            max_messages: None,
        }
    }

    #[test]
    fn the_limit_is_a_share_of_what_the_model_holds_unless_configured() {
        assert_eq!(config(None).limit(Some(16_384)), 12_288);
        assert_eq!(config(None).limit(None), DEFAULT_MAX_TOKENS);
        assert_eq!(config(Some(5000)).limit(Some(16_384)), 5000);
        assert_eq!(config(Some(5000)).limit(None), 5000);
    }
}
