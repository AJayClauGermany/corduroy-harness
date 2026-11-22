//! Services: calls from one plugin to another, in JSON text.
//!
//! The plugin tree already lets native plugins share typed values: one plugin provides a
//! value under a name and others inject it. That cannot cross the WebAssembly boundary. A
//! [`Service`] can: it is called by name, with a method and a payload that are text (JSON by
//! convention), and it is told which plugin is calling. Native and WebAssembly plugins both
//! register services in the [`Services`] registry and both call them, so a plugin can offer
//! an extension point of its own (an MCP plugin accepting server definitions, say) without
//! the core knowing about it.
//!
//! **Authority does not travel.** A service acts with the grants of the plugin that provides
//! it, not the caller's. Providing a service is therefore a deliberate grant of some of the
//! provider's power to whoever may call it; the caller's own grant decides which services
//! it may call, and the provider is told the caller's name and may refuse.

use std::error::Error;
use std::fmt;

use crate::registry::Registry;

/// Name of the service under which the host provides the [`Services`] registry.
pub const SERVICES: &str = "services";

/// The registry of every service plugins provide, by name.
pub type Services = Registry<dyn Service>;

/// Why a call to a service failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// No plugin provides the service, or it has no such method.
    Unknown(String),
    /// The service rejected the payload.
    InvalidInput(String),
    /// The service accepted the call but could not complete it.
    Failed(String),
    /// The caller may not make this call.
    Denied(String),
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(what) => write!(f, "unknown: {what}"),
            Self::InvalidInput(reason) => write!(f, "invalid input: {reason}"),
            Self::Failed(reason) => write!(f, "call failed: {reason}"),
            Self::Denied(reason) => write!(f, "permission denied: {reason}"),
        }
    }
}

impl Error for CallError {}

/// Something plugins can call by name.
pub trait Service: Send + Sync {
    /// Answer `method` with `payload` on behalf of the plugin named `caller`.
    ///
    /// # Errors
    ///
    /// Returns a [`CallError`] if the method is unknown, the payload is invalid, the caller
    /// is refused or the call cannot complete.
    fn call(&self, caller: &str, method: &str, payload: &str) -> Result<String, CallError>;
}

impl<F> Service for F
where
    F: Fn(&str, &str, &str) -> Result<String, CallError> + Send + Sync,
{
    fn call(&self, caller: &str, method: &str, payload: &str) -> Result<String, CallError> {
        self(caller, method, payload)
    }
}

impl Registry<dyn Service> {
    /// Call `method` of the service `name` with `payload`, as `caller`.
    ///
    /// # Errors
    ///
    /// Returns [`CallError::Unknown`] if nothing provides `name`, or whatever the service
    /// returns.
    pub fn call(
        &self,
        caller: &str,
        name: &str,
        method: &str,
        payload: &str,
    ) -> Result<String, CallError> {
        // The registry lock is released before the service runs.
        self.get(name)
            .ok_or_else(|| CallError::Unknown(format!("no plugin provides the service `{name}`")))?
            .call(caller, method, payload)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn calls_reach_the_provider_with_the_callers_name() {
        let services = Services::new();
        let greeter: Arc<dyn Service> =
            Arc::new(|caller: &str, method: &str, payload: &str| match method {
                "greet" => Ok(format!("hello {payload}, from a call by {caller}")),
                _ => Err(CallError::Unknown(format!("method `{method}`"))),
            });
        services.insert("greeter", greeter).unwrap();

        assert_eq!(
            services.call("p", "greeter", "greet", "you"),
            Ok("hello you, from a call by p".to_owned())
        );
        assert_eq!(
            services
                .call("p", "greeter", "wave", "")
                .unwrap_err()
                .to_string(),
            "unknown: method `wave`"
        );
        assert_eq!(
            services.call("p", "missing", "greet", ""),
            Err(CallError::Unknown(
                "no plugin provides the service `missing`".to_owned()
            ))
        );
        assert_eq!(
            CallError::Denied("no".into()).to_string(),
            "permission denied: no"
        );
    }
}
