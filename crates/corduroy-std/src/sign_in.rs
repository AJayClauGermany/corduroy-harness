//! Signing in: what some kinds of secret need a person to do before they can be revealed.
//!
//! A secret read from the environment is there or it is not. One that stands for an account
//! (a `ChatGPT` plan) has to be signed in to first, through a browser, by the person. A
//! [`SignIn`] is how the command does that for one kind of secret; the [`Standard`] holds
//! one per kind that needs it, so a command of one's own can bring others.
//!
//! [`Standard`]: crate::Standard

use std::sync::Arc;

use corduroy_chatgpt::sign_in::{Front, open_in_browser};
use corduroy_chatgpt::{ChatGpt, Record, USAGE_URL, advice};
use corduroy_keychain::Keychain;

use crate::extension::{Asked, chatgpt_in};

/// How a person signs in to, and out of, what one kind of secret stands for.
pub trait SignIn: Send + Sync {
    /// Sign in for the entry `asked`, saying what the person has to know with `say`. It may
    /// take as long as the person does.
    ///
    /// # Errors
    ///
    /// Says why nobody is signed in afterwards.
    fn sign_in(&self, asked: &Asked<'_>, say: &mut dyn FnMut(&str)) -> Result<(), String>;

    /// Sign out for the entry `asked`: what was kept is revoked and forgotten.
    ///
    /// # Errors
    ///
    /// Says why the sign-in is still there.
    fn sign_out(&self, asked: &Asked<'_>, say: &mut dyn FnMut(&str)) -> Result<(), String>;

    /// A line to show the person when a run starts whose model is reached with a secret of
    /// this kind: whose account it is on. None by default.
    fn using(&self) -> Option<String> {
        None
    }

    /// What the person can do about the reason such a run failed, if this kind knows.
    /// Nothing by default.
    fn advise(&self, _failure: &str) -> Option<String> {
        None
    }
}

/// Sign in with `ChatGPT`, through the person's browser.
///
/// The sign-in is kept in the keychain this was made with, or, where its entry says
/// `keep = "file"`, in a file of the person's own.
#[derive(Clone)]
pub struct ChatGptSignIn {
    keychain: Arc<dyn Keychain>,
}

impl std::fmt::Debug for ChatGptSignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatGptSignIn")
            .field("keychain", &self.keychain.name())
            .finish()
    }
}

impl ChatGptSignIn {
    /// Signing in, with `keychain` for the entries that keep their sign-in in one.
    #[must_use]
    pub const fn new(keychain: Arc<dyn Keychain>) -> Self {
        Self { keychain }
    }
}

/// Say whose sign-in `record` is, and whether their plan pays.
fn signed_in(record: &Record, say: &mut dyn FnMut(&str)) -> Result<(), String> {
    let who = record.email.as_deref().unwrap_or("your account");
    say(&format!("Signed in to ChatGPT as {who}."));
    if record.plan() {
        say("You're using your ChatGPT plan: eligible usage here uses it.");
        say(&format!("Manage usage: {USAGE_URL}"));
        Ok(())
    } else {
        Err(
            "the use of your ChatGPT plan was not allowed, so no request can be paid by \
             it: sign in again and allow it"
                .to_owned(),
        )
    }
}

impl SignIn for ChatGptSignIn {
    fn sign_in(&self, asked: &Asked<'_>, say: &mut dyn FnMut(&str)) -> Result<(), String> {
        let chatgpt: ChatGpt = chatgpt_in(asked, &self.keychain)?;
        // A sign-in made while it was kept in a file is moved, not made again.
        if chatgpt.store().adopt()?
            && let Some(record) = chatgpt.store().load()?
        {
            say(&format!(
                "Moved the sign-in from {} into {}.",
                chatgpt.store().file().display(),
                chatgpt.store().place()
            ));
            return signed_in(&record, say);
        }
        let record = chatgpt.sign_in(&mut Front {
            open: &open_in_browser,
            say,
        })?;
        say(&format!(
            "The sign-in is kept in {}.",
            chatgpt.store().place()
        ));
        signed_in(&record, say)
    }

    fn sign_out(&self, asked: &Asked<'_>, say: &mut dyn FnMut(&str)) -> Result<(), String> {
        let chatgpt = chatgpt_in(asked, &self.keychain)?;
        // A sign-in still in the file it was first kept in is the one to revoke.
        chatgpt.store().adopt()?;
        if chatgpt.sign_out()? {
            say("Signed out of ChatGPT: the sign-in was revoked and its tokens removed.");
        } else {
            say("Nobody was signed in to ChatGPT.");
        }
        Ok(())
    }

    fn using(&self) -> Option<String> {
        Some(advice::using())
    }

    fn advise(&self, failure: &str) -> Option<String> {
        advice::advice(failure)
    }
}
