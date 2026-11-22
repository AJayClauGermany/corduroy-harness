//! Models: a language model that plugins can ask for one completion.
//!
//! The agent loop talks to its model through the harness's provider; this registry is how a
//! plugin reaches a model of its own, to summarise, compact or review. Entries are registered
//! by model name in the [`Models`] registry, which the host provides as the service named
//! [`MODELS`]. The core only says what a request and its answer are; whatever registers a
//! model (in `corduroy-harness`, a provider plugin) does the talking.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;
use core::error::Error;
use core::fmt;
use core::time::Duration;

use crate::registry::Registry;

/// Name of the service under which the host provides the [`Models`] registry.
pub const MODELS: &str = "models";

/// The registry of every model plugins may ask, by model name.
pub type Models = Registry<dyn Model>;

/// Who said a [`Turn`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The person, or the plugin speaking for one.
    User,
    /// The model.
    Assistant,
}

/// One message of a [`Completion`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    /// Who said it.
    pub role: Role,
    /// What was said.
    pub text: String,
}

/// One request for a model: a system prompt and a conversation, ending with the user.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Completion {
    /// The system prompt.
    pub system: String,
    /// The conversation so far, oldest first.
    pub messages: Vec<Turn>,
}

/// Why a model gave no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    /// No model is registered under this name.
    Unknown(String),
    /// The model was asked and failed.
    Failed(String),
    /// The model did not answer in time.
    TimedOut(Duration),
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(name) => write!(f, "unknown model `{name}`"),
            Self::Failed(reason) => write!(f, "the model failed: {reason}"),
            Self::TimedOut(limit) => {
                write!(f, "the model did not answer within {}ms", limit.as_millis())
            }
        }
    }
}

impl Error for ModelError {}

/// A model that answers one [`Completion`] at a time with text.
pub trait Model: Send + Sync {
    /// Answer `request`, waiting at most `timeout`.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::Failed`] if the model fails and [`ModelError::TimedOut`] if it
    /// takes longer than `timeout`.
    fn complete(&self, request: &Completion, timeout: Duration) -> Result<String, ModelError>;
}

impl<F> Model for F
where
    F: Fn(&Completion, Duration) -> Result<String, ModelError> + Send + Sync,
{
    fn complete(&self, request: &Completion, timeout: Duration) -> Result<String, ModelError> {
        self(request, timeout)
    }
}

impl Registry<dyn Model> {
    /// Ask the model registered as `name`.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::Unknown`] if there is no such model, or whatever
    /// [`Model::complete`] returns.
    pub fn complete(
        &self,
        name: &str,
        request: &Completion,
        timeout: Duration,
    ) -> Result<String, ModelError> {
        let model = self
            .get(name)
            .ok_or_else(|| ModelError::Unknown(name.to_owned()))?;
        model.complete(request, timeout)
    }
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::string::ToString;
    use alloc::sync::Arc;
    use alloc::vec;

    use super::*;

    #[test]
    fn models_are_asked_by_name() {
        let models = Models::new();
        let echo: Arc<dyn Model> = Arc::new(|request: &Completion, _: Duration| {
            Ok(format!("{} messages", request.messages.len()))
        });
        models.insert("echo", echo).unwrap();
        let request = Completion {
            system: String::new(),
            messages: vec![Turn {
                role: Role::User,
                text: "hi".into(),
            }],
        };
        let wait = Duration::from_secs(1);

        assert_eq!(
            models.complete("echo", &request, wait).unwrap(),
            "1 messages"
        );
        assert_eq!(
            models.complete("other", &request, wait),
            Err(ModelError::Unknown("other".into()))
        );
        assert_eq!(
            ModelError::TimedOut(wait).to_string(),
            "the model did not answer within 1000ms"
        );
    }
}
