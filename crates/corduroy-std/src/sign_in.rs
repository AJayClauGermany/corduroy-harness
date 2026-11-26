//! Signing in: what some kinds of secret need a person to do before they can be revealed.
//!
//! A secret read from the environment is there or it is not. One that stands for an account
//! (a `ChatGPT` plan) has to be signed in to first, through a browser, by the person. A
//! [`SignIn`] is how the command does that for one kind of secret; the [`Standard`] holds
//! one per kind that needs it, so a command of one's own can bring others.
//!
//! [`Standard`]: crate::Standard

use corduroy_chatgpt::sign_in::{Front, open_in_browser};
use corduroy_chatgpt::{ChatGpt, USAGE_URL, advice};

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
#[derive(Debug, Clone, Copy, Default)]
pub struct ChatGptSignIn;

impl SignIn for ChatGptSignIn {
    fn sign_in(&self, asked: &Asked<'_>, say: &mut dyn FnMut(&str)) -> Result<(), String> {
        let chatgpt: ChatGpt = chatgpt_in(asked)?;
        let record = chatgpt.sign_in(&mut Front {
            open: &open_in_browser,
            say,
        })?;
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

    fn sign_out(&self, asked: &Asked<'_>, say: &mut dyn FnMut(&str)) -> Result<(), String> {
        if chatgpt_in(asked)?.sign_out()? {
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
