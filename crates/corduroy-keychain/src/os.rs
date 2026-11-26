//! The system's own keychain.

mod pieces;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use keyring_core::api::CredentialStore;
use keyring_core::{Entry, Error};

use self::pieces::Raw;
use crate::{Keychain, SERVICE, item_name};

/// The keychain of the system this runs on.
///
/// The macOS Keychain, the Windows Credential Manager, or the Secret Service on Linux and
/// other Unix systems. It is reached when first asked, and a machine without one says so
/// then.
#[derive(Default)]
pub struct Os {
    store: OnceLock<Result<Arc<CredentialStore>, String>>,
}

impl std::fmt::Debug for Os {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Os").field("name", &NAME).finish()
    }
}

/// What the system's keychain is called.
#[cfg(target_os = "macos")]
const NAME: &str = "the macOS Keychain";
#[cfg(windows)]
const NAME: &str = "the Windows Credential Manager";
#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
const NAME: &str = "the Secret Service keyring";
#[cfg(not(any(windows, all(unix, not(any(target_os = "ios", target_os = "android"))))))]
const NAME: &str = "no keychain";

/// The most bytes one item takes: the Credential Manager's 2560, less room to spare.
#[cfg(windows)]
const MOST: Option<usize> = Some(2048);
#[cfg(not(windows))]
const MOST: Option<usize> = None;

#[cfg(target_os = "macos")]
fn open() -> Result<Arc<CredentialStore>, String> {
    let store: Arc<CredentialStore> =
        apple_native_keyring_store::keychain::Store::new().map_err(|error| why(&error))?;
    Ok(store)
}

#[cfg(windows)]
fn open() -> Result<Arc<CredentialStore>, String> {
    let store: Arc<CredentialStore> =
        windows_native_keyring_store::Store::new().map_err(|error| why(&error))?;
    Ok(store)
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
fn open() -> Result<Arc<CredentialStore>, String> {
    let store: Arc<CredentialStore> =
        zbus_secret_service_keyring_store::Store::new().map_err(|error| {
            format!(
                "the Secret Service did not answer, as on a machine nobody is logged in to, or \
                 in a container: {}",
                why(&error)
            )
        })?;
    Ok(store)
}

#[cfg(not(any(windows, all(unix, not(any(target_os = "ios", target_os = "android"))))))]
fn open() -> Result<Arc<CredentialStore>, String> {
    Err("this system has none".to_owned())
}

/// What went wrong, for a person. A keychain's errors may carry the bytes it held, which
/// are never repeated.
fn why(error: &Error) -> String {
    match error {
        Error::NoEntry => "there is no such item".to_owned(),
        Error::BadEncoding(_) | Error::BadDataFormat(..) => {
            "what is kept there is not text".to_owned()
        }
        Error::NoStorageAccess(cause) => {
            format!("the keychain is locked or refused access: {cause}")
        }
        Error::PlatformFailure(cause) => cause.to_string(),
        Error::TooLong(what, most) => format!("the {what} is longer than {most}"),
        Error::Ambiguous(found) => {
            format!("{} items go by that name: remove all but one", found.len())
        }
        other => other.to_string(),
    }
}

impl Os {
    /// The system's keychain, not yet asked for anything.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            store: OnceLock::new(),
        }
    }

    fn store(&self) -> Result<&Arc<CredentialStore>, String> {
        self.store
            .get_or_init(open)
            .as_ref()
            .map_err(|why| format!("this machine has no keychain ({why})"))
    }

    /// The entry of `item`, which may be a piece of one.
    fn entry(&self, item: &str) -> Result<Entry, String> {
        let label = format!("{SERVICE}: {item}");
        // Only the Secret Service shows a label of its own beside the name.
        let modifiers: HashMap<&str, &str> = if cfg!(any(windows, target_os = "macos")) {
            HashMap::new()
        } else {
            HashMap::from([("label", label.as_str())])
        };
        self.store()?
            .build(SERVICE, item, Some(&modifiers))
            .map_err(|error| format!("{NAME}: {}", why(&error)))
    }
}

impl Raw for Os {
    fn read(&self, item: &str) -> Result<Option<Vec<u8>>, String> {
        match self.entry(item)?.get_secret() {
            Ok(value) => Ok(Some(value)),
            Err(Error::NoEntry) => Ok(None),
            Err(error) => Err(format!("{NAME} did not give `{item}`: {}", why(&error))),
        }
    }

    fn write(&self, item: &str, value: &[u8]) -> Result<(), String> {
        self.entry(item)?
            .set_secret(value)
            .map_err(|error| format!("{NAME} did not take `{item}`: {}", why(&error)))
    }

    fn erase(&self, item: &str) -> Result<bool, String> {
        match self.entry(item)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(Error::NoEntry) => Ok(false),
            Err(error) => Err(format!("{NAME} did not remove `{item}`: {}", why(&error))),
        }
    }
}

impl Keychain for Os {
    fn name(&self) -> String {
        NAME.to_owned()
    }

    fn reachable(&self) -> Result<(), String> {
        self.store().map(|_| ())
    }

    fn get(&self, item: &str) -> Result<Option<String>, String> {
        pieces::get(self, item_name(item)?, MOST)?
            .map(|value| {
                String::from_utf8(value)
                    .map_err(|_| format!("what {NAME} keeps as `{item}` is not text"))
            })
            .transpose()
    }

    fn set(&self, item: &str, value: &str) -> Result<(), String> {
        pieces::set(self, item_name(item)?, value.as_bytes(), MOST)
    }

    fn remove(&self, item: &str) -> Result<bool, String> {
        pieces::remove(self, item_name(item)?, MOST)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether the test of the real keychain runs: only where `CORDUROY_REQUIRE_KEYCHAIN`
    /// is set, as in CI. A person's own keychain is not something a test run touches
    /// unasked.
    fn required() -> bool {
        std::env::var_os("CORDUROY_REQUIRE_KEYCHAIN").is_some()
    }

    #[test]
    fn the_systems_keychain_keeps_a_value_and_forgets_it() {
        if !required() {
            return;
        }
        let keychain = Os::new();
        if let Err(why) = keychain.reachable() {
            panic!("a keychain is required here: {why}");
        }
        let item = format!("corduroy-test-{}", std::process::id());
        assert_eq!(keychain.get(&item).unwrap(), None);
        assert!(!keychain.remove(&item).unwrap());

        keychain.set(&item, "first").unwrap();
        assert_eq!(keychain.get(&item).unwrap().as_deref(), Some("first"));
        // Set again, it is replaced and not kept twice. Longer than the Credential Manager
        // takes in one item, it is still one value.
        let long = "tökén.".repeat(1000);
        keychain.set(&item, &long).unwrap();
        assert_eq!(keychain.get(&item).unwrap().as_deref(), Some(long.as_str()));
        keychain.set(&item, "second").unwrap();
        assert_eq!(keychain.get(&item).unwrap().as_deref(), Some("second"));

        assert!(keychain.remove(&item).unwrap());
        assert_eq!(keychain.get(&item).unwrap(), None);
    }
}
