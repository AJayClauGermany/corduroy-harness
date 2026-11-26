//! Sign in with `ChatGPT`: a person's `ChatGPT` plan in place of an API key.
//!
//! `OpenAI` lets an open-source tool ask a person, through their browser, to let it use their
//! `ChatGPT` plan for requests to the Responses API
//! (<https://developers.openai.com/siwc/token-sharing-open-source>). What comes back is an
//! access token good for an hour and a refresh token that renews it. This crate is that
//! flow, and what the harness needs around it:
//!
//! - [`ChatGpt::sign_in`] registers this tool as a client on first use, sends the person to
//!   the browser, takes the answer on the loopback interface, checks the ID token and keeps
//!   the tokens in a [`Store`]: a file of the person's own, outside every workspace.
//! - [`ChatGpt::token`] gives the access token, renewing it first when it is about to run
//!   out. Renewal replaces the refresh token, so processes take turns through a lock.
//! - [`chatgpt_secret`] is that token as a [`Secret`](corduroy_core::Secret): the plugin an
//!   agent file's `[secrets.<name>]` entry of `kind = "chatgpt"` asks for. A provider entry
//!   that names it sends the token of the moment with every request.
//!
//! The tokens are never printed, logged or put in an error. No client secret and no partner
//! key are involved: the flow is the public-client one, with PKCE.

pub mod id_token;
pub mod random;
pub mod secret;
pub mod sign_in;
pub mod store;
pub mod token;
pub mod web;

use std::sync::Arc;

use corduroy_core::HttpClient;

pub use secret::chatgpt_secret;
pub use store::{Record, Store};

use crate::web::Web;

/// The API the tokens are for.
pub const RESOURCE: &str = "https://api.openai.com/v1";

/// What is asked for at sign-in: who the person is, a refresh token, and the use of their
/// plan.
pub const SCOPE: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

/// The scope without which a token is not good for requests paid by the plan.
pub const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";

/// Where a person sees and limits what this tool uses of their plan.
pub const USAGE_URL: &str = "https://chatgpt.com/settings/usage";

/// The name this tool registers under, the same on every installation.
pub const APP_NAME: &str = "Corduroy";

/// The command a person is sent to when they have to sign in (again).
pub(crate) const SIGN_IN: &str = "corduroy login";

/// Where `OpenAI`'s sign-in is.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Endpoints {
    /// Where the browser is sent.
    pub authorize: String,
    /// Where a code, or a refresh token, is exchanged for tokens.
    pub token: String,
    /// The discovery document, which says where the signing keys and revocation are.
    pub discovery: String,
    /// Who issues the tokens: the `iss` an ID token must carry.
    pub issuer: String,
}

impl Endpoints {
    /// `OpenAI`'s own.
    #[must_use]
    pub fn openai() -> Self {
        Self::at("https://auth.openai.com")
    }

    /// The same paths under `issuer`, for an issuer that is not `OpenAI`'s (a test's).
    #[must_use]
    pub fn at(issuer: &str) -> Self {
        Self {
            authorize: format!("{issuer}/api/accounts/authorize"),
            token: format!("{issuer}/api/accounts/oauth/token"),
            discovery: format!("{issuer}/.well-known/openid-configuration"),
            issuer: issuer.to_owned(),
        }
    }
}

/// A `ChatGPT` sign-in: where its tokens are kept and whom it talks to.
#[derive(Clone)]
pub struct ChatGpt {
    pub(crate) endpoints: Endpoints,
    pub(crate) store: Store,
    pub(crate) web: Web,
}

impl std::fmt::Debug for ChatGpt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatGpt")
            .field("endpoints", &self.endpoints)
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

impl ChatGpt {
    /// A sign-in with `OpenAI`, its tokens kept in `store`.
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self {
            endpoints: Endpoints::openai(),
            store,
            web: Web::new(Arc::new(corduroy_http::UreqClient::new())),
        }
    }

    /// Talking to `endpoints` instead of `OpenAI`'s.
    #[must_use]
    pub fn with_endpoints(mut self, endpoints: Endpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    /// Sending requests through `client`.
    #[must_use]
    pub fn with_client(mut self, client: Arc<dyn HttpClient>) -> Self {
        self.web = Web::new(client);
        self
    }

    /// Where the tokens are kept.
    #[must_use]
    pub const fn store(&self) -> &Store {
        &self.store
    }
}

/// Seconds since the Unix epoch.
pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}
