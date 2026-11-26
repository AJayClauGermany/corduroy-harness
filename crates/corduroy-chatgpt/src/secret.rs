//! The access token as a secret: the token of the moment, renewed as it runs out.

use std::sync::{Arc, Mutex};

use corduroy_core::{FnPlugin, PluginError, SECRETS, Secret, Secrets, plugin_fn};

use crate::store::Record;
use crate::token::{self, Refused};
use crate::{ChatGpt, SIGN_IN, now};

impl ChatGpt {
    /// The access token, renewed first if it is about to run out.
    ///
    /// A renewal is one process's at a time, and its tokens are kept before they are used:
    /// the refresh token it spends is replaced by the one it gets. A renewal that fails for
    /// now leaves a token that is still good in use.
    ///
    /// # Errors
    ///
    /// If nobody is signed in, the person did not let this tool use their plan, or the
    /// token has run out and cannot be renewed. Never holds a token.
    pub fn token(&self) -> Result<String, String> {
        self.current().map(|record| record.access_token)
    }

    /// The sign-in, with an access token that is good.
    fn current(&self) -> Result<Record, String> {
        let record = self.signed_in()?;
        if record.fresh(now()) || !record.renewable(now()) && record.usable(now()) {
            return Ok(record);
        }
        let _held = self.store.lock()?;
        // Another process may have renewed it while this one waited.
        let record = self.signed_in()?;
        if record.fresh(now()) {
            return Ok(record);
        }
        match token::refresh(&self.web, &self.endpoints, &record) {
            Ok(renewed) => {
                self.store.save(&renewed)?;
                Ok(renewed)
            }
            Err(Refused::ForNow(_)) if record.usable(now()) => Ok(record),
            Err(refused) => Err(refused.to_string()),
        }
    }

    fn signed_in(&self) -> Result<Record, String> {
        let record = self
            .store
            .load()?
            .ok_or_else(|| format!("nobody is signed in to ChatGPT: run `{SIGN_IN}`"))?;
        if record.plan() {
            Ok(record)
        } else {
            Err(format!(
                "the ChatGPT sign-in does not allow the use of the plan: run `{SIGN_IN}` and \
                 allow it"
            ))
        }
    }
}

/// A sign-in's access token, with the last one read kept so that a request does not read
/// the file while the token is good.
struct Token {
    chatgpt: ChatGpt,
    last: Mutex<Option<Record>>,
}

impl Secret for Token {
    fn reveal(&self) -> Result<String, String> {
        let poisoned = |_| "the ChatGPT sign-in is unusable after a failure".to_owned();
        let kept = self
            .last
            .lock()
            .map_err(poisoned)?
            .as_ref()
            .filter(|record| record.fresh(now()))
            .map(|record| record.access_token.clone());
        if let Some(token) = kept {
            return Ok(token);
        }
        // Renewing may take a while, and is not done holding the lock: two requests at once
        // both go to the store, which lets one of them renew.
        let (token, record) = match self.chatgpt.current() {
            Ok(record) => (Ok(record.access_token.clone()), Some(record)),
            Err(why) => (Err(why), None),
        };
        *self.last.lock().map_err(poisoned)? = record;
        token
    }
}

/// The plugin that registers `chatgpt`'s access token as the secret `name`.
///
/// Nothing is asked of the service when it is applied: a sign-in that is missing or spent
/// shows when the secret is first revealed, with the command that mends it.
pub fn chatgpt_secret(
    name: impl Into<String>,
    chatgpt: ChatGpt,
) -> FnPlugin<impl FnMut(&mut corduroy_core::Context<'_>) -> Result<(), PluginError> + Send + Sync>
{
    let name = name.into();
    plugin_fn(format!("secret-{name}"), &[SECRETS], move |ctx| {
        let secret: Arc<dyn Secret> = Arc::new(Token {
            chatgpt: chatgpt.clone(),
            last: Mutex::new(None),
        });
        ctx.service::<Secrets>(SECRETS)?
            .register(ctx, name.clone(), secret)?;
        Ok(())
    })
}
