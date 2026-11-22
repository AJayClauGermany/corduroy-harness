//! Secrets: credentials plugins may use without seeing.
//!
//! A secret is registered by name in the [`Secrets`] registry, which the host provides as the
//! service named [`SECRETS`]. Native plugins (a keychain, an environment variable, a vault
//! client) register them; a WebAssembly plugin granted the `secret` capability gets only an
//! opaque handle, which host functions such as `http` write into a request. The value never
//! enters the event stream: what is recorded is that the secret was used, by name.

use crate::registry::Registry;
use alloc::format;
use alloc::string::String;

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
}

#[cfg(test)]
mod tests {
    use alloc::borrow::ToOwned;
    use alloc::sync::Arc;

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
}
