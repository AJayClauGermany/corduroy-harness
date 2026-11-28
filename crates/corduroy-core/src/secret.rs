//! Secrets: credentials plugins may use without seeing.
//!
//! A secret is registered by name in the [`Secrets`] registry, which the host provides as the
//! service named [`SECRETS`]. Native plugins (a keychain, an environment variable, a vault
//! client) register them; a WebAssembly plugin granted the `secret` capability gets only an
//! opaque handle, which host functions such as `http` write into a request. The value never
//! enters the event stream: what is recorded is that the secret was used, by name.
//!
//! **A secret may name the hosts it goes to.** That a secret names its hosts, and that one
//! naming none goes nowhere, is decided; the form is not (B9 in `docs/open-questions.md`),
//! and what is here is a prototype of one: [`Secret::hosts`], a scope over host names as an
//! `http` grant is, checked by whatever writes a value into a request
//! ([`Secrets::may_go`]). Until the form is chosen a secret that says nothing is sent as
//! before, wherever the grants of what uses it reach. `docs/prototypes.md` says what this
//! is for and what it has found.

use crate::capability::Scope;
use crate::registry::Registry;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;

/// Name of the service under which the host provides the [`Secrets`] registry.
pub const SECRETS: &str = "secrets";

/// The registry of every secret, by name.
pub type Secrets = Registry<dyn Secret>;

/// Something that yields a credential when the host needs it.
pub trait Secret: Send + Sync {
    /// The credential's current value.
    ///
    /// # Errors
    ///
    /// Returns why the value could not be had, without the value.
    fn reveal(&self) -> Result<String, String>;

    /// The hosts this credential may be sent to, if it says: a scope over host names, in
    /// lowercase and without a port. `None`, the default, is a secret that does not say.
    ///
    /// A prototype (see [the module](self)): a secret that does not say is sent as it was
    /// before there was this method, and is to go nowhere once the form is settled.
    fn hosts(&self) -> Option<Scope> {
        None
    }
}

/// A secret held to the hosts it names: `secret`, going only to `hosts`.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use corduroy_core::{Scope, Secret, Secrets, secret};
///
/// let key: Arc<dyn Secret> = Arc::new(|| Ok("s3cret".to_owned()));
/// let secrets = Secrets::new();
/// secrets.insert("key", secret::to_hosts(key, Scope::none().allow("api.example.com")))?;
///
/// assert!(secrets.may_go("key", "api.example.com").is_ok());
/// assert!(secrets.may_go("key", "elsewhere.example").is_err());
/// # Ok::<(), corduroy_core::Duplicate>(())
/// ```
#[must_use]
pub fn to_hosts(secret: Arc<dyn Secret>, hosts: Scope) -> Arc<dyn Secret> {
    Arc::new(ToHosts { secret, hosts })
}

/// See [`to_hosts`].
struct ToHosts {
    secret: Arc<dyn Secret>,
    hosts: Scope,
}

impl Secret for ToHosts {
    fn reveal(&self) -> Result<String, String> {
        self.secret.reveal()
    }

    fn hosts(&self) -> Option<Scope> {
        Some(self.hosts.clone())
    }
}

/// The host an absolute URL names, in lowercase and without its port, as a secret's
/// [`hosts`](Secret::hosts) are written. `None` for anything else, a URL with a user name
/// included.
#[must_use]
pub fn host_of(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.contains('@') {
        return None;
    }
    // An IPv6 address is bracketed: `[::1]:8080`.
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => bracketed.split_once(']')?.0,
        None => authority.split(':').next()?,
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

impl<F> Secret for F
where
    F: Fn() -> Result<String, String> + Send + Sync,
{
    fn reveal(&self) -> Result<String, String> {
        self()
    }
}

impl Registry<dyn Secret> {
    /// The value of the secret registered as `name`.
    ///
    /// # Errors
    ///
    /// Returns why there is no value: no such secret, or whatever [`Secret::reveal`] returns.
    pub fn reveal(&self, name: &str) -> Result<String, String> {
        self.get(name)
            .ok_or_else(|| format!("no secret is registered as `{name}`"))?
            .reveal()
    }

    /// Whether the secret registered as `name` may be sent to `host`: it names the host
    /// among its [`hosts`](Secret::hosts), or does not say where it goes. Whatever writes a
    /// secret's value into a request asks this first, with the host the request goes to.
    ///
    /// # Errors
    ///
    /// Returns why not: no such secret, or the secret names its hosts and `host` is not one.
    pub fn may_go(&self, name: &str, host: &str) -> Result<(), String> {
        let secret = self
            .get(name)
            .ok_or_else(|| format!("no secret is registered as `{name}`"))?;
        match secret.hosts() {
            Some(hosts) if !hosts.permits(&host.to_ascii_lowercase()) => Err(format!(
                "secret `{name}` does not name `{host}` among the hosts it may be sent to"
            )),
            Some(_) | None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::borrow::ToOwned;

    use super::*;

    #[test]
    fn secrets_are_revealed_by_name() {
        let secrets = Secrets::new();
        let token: Arc<dyn Secret> = Arc::new(|| Ok("s3cret".to_owned()));
        secrets.insert("token", token).unwrap();

        assert_eq!(secrets.reveal("token").unwrap(), "s3cret");
        assert_eq!(
            secrets.reveal("other").unwrap_err(),
            "no secret is registered as `other`"
        );
    }

    #[test]
    fn a_secret_that_names_its_hosts_goes_to_no_other() {
        let secrets = Secrets::new();
        let token: Arc<dyn Secret> = Arc::new(|| Ok("s3cret".to_owned()));
        let hosts = Scope::none()
            .allow("api.example.com")
            .allow("*.example.org");
        secrets
            .insert("named", to_hosts(Arc::clone(&token), hosts))
            .unwrap();
        secrets
            .insert("nowhere", to_hosts(Arc::clone(&token), Scope::none()))
            .unwrap();
        secrets.insert("unsaid", token).unwrap();

        assert!(secrets.may_go("named", "api.example.com").is_ok());
        assert!(secrets.may_go("named", "API.Example.com").is_ok());
        assert!(secrets.may_go("named", "eu.example.org").is_ok());
        assert_eq!(
            secrets.may_go("named", "evil.example").unwrap_err(),
            "secret `named` does not name `evil.example` among the hosts it may be sent to"
        );
        // Naming none is not the same as not saying.
        assert!(secrets.may_go("nowhere", "api.example.com").is_err());
        assert!(secrets.may_go("unsaid", "anywhere.example").is_ok());
        assert!(secrets.may_go("missing", "api.example.com").is_err());
        assert_eq!(secrets.reveal("named").unwrap(), "s3cret");
    }

    #[test]
    fn a_url_names_its_host_without_port_or_case() {
        for (url, host) in [
            ("https://API.Example.com/v1/x?y#z", Some("api.example.com")),
            ("http://localhost:8080", Some("localhost")),
            ("http://[::1]:8080/x", Some("::1")),
            ("https://user@example.com/", None),
            ("example.com/path", None),
            ("https:///path", None),
        ] {
            assert_eq!(host_of(url).as_deref(), host, "{url}");
        }
    }
}
