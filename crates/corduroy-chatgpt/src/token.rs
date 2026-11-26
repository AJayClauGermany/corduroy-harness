//! The token endpoint: a code for tokens, a refresh token for new ones, and revocation.

use serde::Deserialize;
use serde_json::Value;

use crate::store::Record;
use crate::web::{Reply, Web};
use crate::{Endpoints, RESOURCE, SIGN_IN, now};

/// What the token endpoint answers with.
#[derive(Clone, Deserialize)]
pub struct Tokens {
    /// What requests are made with.
    pub access_token: String,
    /// What renews it: there when `offline_access` was granted.
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Who signed in. Not always there at a renewal.
    #[serde(default)]
    pub id_token: Option<String>,
    /// `Bearer`.
    #[serde(default)]
    pub token_type: Option<String>,
    /// How long the access token is good for, in seconds.
    #[serde(default)]
    pub expires_in: Option<u64>,
    /// What was granted, separated by spaces.
    #[serde(default)]
    pub scope: Option<String>,
    /// When the service allows the next renewal.
    #[serde(default)]
    pub earliest_refresh_at: Option<Value>,
}

impl std::fmt::Debug for Tokens {
    // The tokens are left out.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("scope", &self.scope)
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

impl Tokens {
    /// What was granted, one scope each.
    #[must_use]
    pub fn scopes(&self) -> Vec<String> {
        self.scope
            .as_deref()
            .unwrap_or("")
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    }
}

fn tokens_of(reply: &Reply) -> Result<Tokens, String> {
    serde_json::from_value(reply.body.clone())
        .map_err(|_| "the token endpoint answered with no tokens".to_owned())
}

/// What a sign-in came back with, and what it was sent with.
#[derive(Clone, Copy)]
pub struct Exchange<'a> {
    /// The client the code was issued to.
    pub client_id: &'a str,
    /// The code.
    pub code: &'a str,
    /// The PKCE verifier whose challenge the sign-in was sent with.
    pub verifier: &'a str,
    /// The redirect URI the sign-in was sent with, exactly.
    pub redirect: &'a str,
}

impl std::fmt::Debug for Exchange<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Exchange")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

/// Exchange the code a sign-in came back with for tokens.
///
/// # Errors
///
/// If the request fails or the code is refused.
pub fn exchange(web: &Web, endpoints: &Endpoints, code: &Exchange<'_>) -> Result<Tokens, String> {
    let reply = web.post(
        &endpoints.token,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", code.client_id),
            ("code", code.code),
            ("code_verifier", code.verifier),
            ("redirect_uri", code.redirect),
            ("resource", RESOURCE),
        ],
    )?;
    if !reply.ok() {
        return Err(format!(
            "the sign-in's code was refused: {}",
            reply.refusal()
        ));
    }
    tokens_of(&reply)
}

/// Why a renewal did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// The refresh token is no good any more: the person has to sign in again.
    SignIn(String),
    /// Something that may pass: the network, the service. The tokens are kept.
    ForNow(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SignIn(why) => write!(
                f,
                "the ChatGPT sign-in is no longer good ({why}): sign in again with `{SIGN_IN}`"
            ),
            Self::ForNow(why) => write!(f, "the ChatGPT sign-in could not be renewed: {why}"),
        }
    }
}

/// The codes with which the service says a refresh token is spent, revoked or unknown.
const SPENT: [&str; 6] = [
    "invalid_grant",
    "invalid_refresh_token",
    "token_expired",
    "refresh_token_expired",
    "refresh_token_invalidated",
    "refresh_token_reused",
];

/// Renew `record`: its refresh token for a new access token, and a new refresh token, which
/// replaces the old one. The scopes stay what they were unless the service says otherwise.
///
/// # Errors
///
/// A [`Refused`]: for good, or for now.
pub fn refresh(web: &Web, endpoints: &Endpoints, record: &Record) -> Result<Record, Refused> {
    let reply = web
        .post(
            &endpoints.token,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", &record.client_id),
                ("refresh_token", &record.refresh_token),
                ("resource", RESOURCE),
            ],
        )
        .map_err(Refused::ForNow)?;
    if !reply.ok() {
        return Err(if SPENT.contains(&reply.code()) {
            Refused::SignIn(reply.refusal())
        } else {
            Refused::ForNow(reply.refusal())
        });
    }
    let tokens = tokens_of(&reply).map_err(Refused::ForNow)?;
    let scopes = tokens.scopes();
    Ok(Record {
        id_token: tokens.id_token.unwrap_or_else(|| record.id_token.clone()),
        access_token: tokens.access_token,
        refresh_token: tokens
            .refresh_token
            .unwrap_or_else(|| record.refresh_token.clone()),
        token_type: tokens
            .token_type
            .unwrap_or_else(|| record.token_type.clone()),
        expires_in: tokens.expires_in.unwrap_or(record.expires_in),
        scopes: if scopes.is_empty() {
            record.scopes.clone()
        } else {
            scopes
        },
        saved_at: now(),
        earliest_refresh_at: tokens.earliest_refresh_at,
        ..record.clone()
    })
}

/// Revoke `record`'s refresh token, so that it renews nothing any more. A token already
/// revoked is revoked.
///
/// # Errors
///
/// If the service names no revocation endpoint, or the request fails or is refused.
pub fn revoke(web: &Web, endpoints: &Endpoints, record: &Record) -> Result<(), String> {
    let discovery = web.get(&endpoints.discovery)?;
    let endpoint = discovery
        .get("revocation_endpoint")
        .and_then(Value::as_str)
        .ok_or("the sign-in service names no revocation endpoint")?;
    let reply = web.post(
        endpoint,
        &[
            ("token", &record.refresh_token),
            ("token_type_hint", "refresh_token"),
            ("client_id", &record.client_id),
        ],
    )?;
    if reply.ok() {
        Ok(())
    } else {
        Err(format!("revocation was refused: {}", reply.refusal()))
    }
}
