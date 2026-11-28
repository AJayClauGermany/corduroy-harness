//! A provider's credential, taken from the [`Secrets`] registry for every request.

use std::sync::Arc;

use anyhow::anyhow;
use async_trait::async_trait;
use corduroy_harness::core::Secrets;
use oi_kit_providers::api_client::{AuthMethod, AuthProvider};

/// How a credential is written into a request.
enum Scheme {
    /// `Authorization: Bearer <value>`.
    Bearer,
    /// `x-api-key: <value>`.
    ApiKey,
}

/// The secret named `name`, revealed each time a request is sent.
struct FromSecret {
    secrets: Arc<Secrets>,
    name: String,
    scheme: Scheme,
    /// The host the provider's requests go to, where its address names one.
    host: Option<String>,
}

#[async_trait]
impl AuthProvider for FromSecret {
    async fn get_auth_header(&self) -> anyhow::Result<(String, String)> {
        // Revealing may take a while (a token renewed over the network), so it is not done
        // on a thread the executor needs. The reason never holds the value:
        // `Secret::reveal` promises as much.
        let (secrets, name, host) = (
            Arc::clone(&self.secrets),
            self.name.clone(),
            self.host.clone(),
        );
        // A secret that names the hosts it may go to is not written into a request to
        // another: an agent file cannot point a key somewhere its owner did not say.
        // The value is had first, so that a secret that cannot be read says why itself.
        let reveal = move || {
            let value = secrets.reveal(&name)?;
            if let Some(host) = &host {
                secrets.may_go(&name, host)?;
            }
            Ok::<_, String>(value)
        };
        let value = tokio::task::spawn_blocking(reveal)
            .await
            .map_err(|_| anyhow!("secret `{}` could not be revealed", self.name))?
            .map_err(|why| anyhow!("secret `{}`: {why}", self.name))?;
        Ok(match self.scheme {
            Scheme::Bearer => ("Authorization".to_owned(), format!("Bearer {value}")),
            Scheme::ApiKey => ("x-api-key".to_owned(), value),
        })
    }
}

/// The secret `name` of `secrets`, sent to `host` as a bearer token.
pub(crate) fn bearer(secrets: Arc<Secrets>, name: String, host: Option<String>) -> AuthMethod {
    AuthMethod::Custom(Box::new(FromSecret {
        secrets,
        name,
        scheme: Scheme::Bearer,
        host,
    }))
}

/// The secret `name` of `secrets`, sent to `host` as Anthropic's `x-api-key`.
pub(crate) fn api_key(secrets: Arc<Secrets>, name: String, host: Option<String>) -> AuthMethod {
    AuthMethod::Custom(Box::new(FromSecret {
        secrets,
        name,
        scheme: Scheme::ApiKey,
        host,
    }))
}
