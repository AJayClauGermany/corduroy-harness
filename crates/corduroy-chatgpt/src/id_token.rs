//! The ID token: who signed in, once its signature and claims have been checked.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use serde_json::Value;

use crate::Endpoints;
use crate::web::Web;

/// Who signed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The account: the token's `sub`.
    pub subject: String,
    /// The person's email, if the token gives one.
    pub email: Option<String>,
    /// Who issued the token.
    pub issuer: String,
}

/// What a sign-in expects of the ID token it gets back.
#[derive(Debug, Clone, Copy)]
pub struct Expected<'a> {
    /// The client the token must be for: its `aud`.
    pub client_id: &'a str,
    /// The `nonce` sent with this attempt.
    pub nonce: &'a str,
    /// The time, in seconds since the Unix epoch.
    pub now: u64,
}

fn part(text: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD
        .decode(text.trim_end_matches('='))
        .map_err(|_| "the ID token is not a signed token".to_owned())
}

fn json(text: &str) -> Result<Value, String> {
    serde_json::from_slice(&part(text)?)
        .map_err(|_| "the ID token is not a signed token".to_owned())
}

/// The key the issuer signs with under `kid`.
fn key(
    web: &Web,
    endpoints: &Endpoints,
    kid: &str,
) -> Result<RsaPublicKeyComponents<Vec<u8>>, String> {
    let discovery = web.get(&endpoints.discovery)?;
    let jwks = discovery
        .get("jwks_uri")
        .and_then(Value::as_str)
        .ok_or("the sign-in service names no signing keys")?;
    let keys = web.get(jwks)?;
    let found = keys
        .get("keys")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|key| {
            key.get("kid").and_then(Value::as_str) == Some(kid)
                && key.get("kty").and_then(Value::as_str) == Some("RSA")
        })
        .ok_or("the ID token is signed with a key the sign-in service does not publish")?;
    let number = |name: &str| {
        found
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| "a signing key is incomplete".to_owned())
            .and_then(part)
    };
    Ok(RsaPublicKeyComponents {
        n: number("n")?,
        e: number("e")?,
    })
}

/// Check `token`, and say who it is for.
///
/// The signature is checked against the key the issuer publishes (RS256, the one algorithm
/// it signs with), then the claims: the issuer, the audience, that it has not run out, and
/// the nonce of this attempt.
///
/// # Errors
///
/// Says which check failed.
pub fn verify(
    web: &Web,
    endpoints: &Endpoints,
    token: &str,
    expected: &Expected<'_>,
) -> Result<Identity, String> {
    let mut parts = token.split('.');
    let (Some(header), Some(claims), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("the ID token is not a signed token".to_owned());
    };
    let described = json(header)?;
    if described.get("alg").and_then(Value::as_str) != Some("RS256") {
        return Err("the ID token is not signed with RS256".to_owned());
    }
    let kid = described.get("kid").and_then(Value::as_str).unwrap_or("");
    let signed = format!("{header}.{claims}");
    key(web, endpoints, kid)?
        .verify(
            &RSA_PKCS1_2048_8192_SHA256,
            signed.as_bytes(),
            &part(signature)?,
        )
        .map_err(|_| "the ID token's signature is not the sign-in service's".to_owned())?;
    identity(&json(claims)?, &endpoints.issuer, expected)
}

/// The claims of an ID token whose signature holds, checked.
fn identity(claims: &Value, issuer: &str, expected: &Expected<'_>) -> Result<Identity, String> {
    let text = |name: &str| claims.get(name).and_then(Value::as_str);
    if text("iss") != Some(issuer) {
        return Err("the ID token is from another issuer".to_owned());
    }
    let audience = claims.get("aud");
    let ours = audience.and_then(Value::as_str) == Some(expected.client_id)
        || audience.and_then(Value::as_array).is_some_and(|all| {
            all.iter()
                .any(|one| one.as_str() == Some(expected.client_id))
        });
    if !ours {
        return Err("the ID token is for another client".to_owned());
    }
    if claims
        .get("exp")
        .and_then(Value::as_u64)
        .is_none_or(|exp| exp <= expected.now)
    {
        return Err("the ID token has run out".to_owned());
    }
    if text("nonce") != Some(expected.nonce) {
        return Err("the ID token is not for this sign-in".to_owned());
    }
    let subject = text("sub").filter(|subject| !subject.is_empty());
    Ok(Identity {
        subject: subject.ok_or("the ID token names no account")?.to_owned(),
        email: text("email").map(str::to_owned),
        issuer: issuer.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const EXPECTED: Expected<'_> = Expected {
        client_id: "oaiapp_1",
        nonce: "n0nce",
        now: 1000,
    };

    fn claims() -> Value {
        json!({"iss": "https://issuer", "aud": "oaiapp_1", "exp": 2000, "nonce": "n0nce",
               "sub": "user-1", "email": "person@example.com"})
    }

    #[test]
    fn claims_that_hold_say_who_signed_in() {
        let who = identity(&claims(), "https://issuer", &EXPECTED).unwrap();
        assert_eq!(who.subject, "user-1");
        assert_eq!(who.email.as_deref(), Some("person@example.com"));
        let mut several = claims();
        several["aud"] = json!(["other", "oaiapp_1"]);
        assert!(identity(&several, "https://issuer", &EXPECTED).is_ok());
    }

    #[test]
    fn each_claim_is_checked() {
        for (name, value, said) in [
            ("iss", json!("https://other"), "another issuer"),
            ("aud", json!("oaiapp_2"), "another client"),
            ("exp", json!(1000), "run out"),
            ("nonce", json!("other"), "not for this sign-in"),
            ("sub", json!(""), "no account"),
        ] {
            let mut changed = claims();
            changed[name] = value;
            let error = identity(&changed, "https://issuer", &EXPECTED).unwrap_err();
            assert!(error.contains(said), "{name}: {error}");
        }
    }
}
