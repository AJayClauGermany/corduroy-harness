//! The operating system's keychain: where a credential is kept encrypted behind the person's
//! own login, and not in a file or an environment variable every program can read.
//!
//! | System | What holds the value | Where a person sees it |
//! | --- | --- | --- |
//! | macOS | the login Keychain, as a generic password | Keychain Access, service `corduroy` |
//! | Windows | the Credential Manager, as a generic credential | Credential Manager, `<item>.corduroy` |
//! | Linux and other Unix | the Secret Service, over D-Bus (GNOME Keyring, `KWallet`, `KeePassXC`) | Seahorse, or `secret-tool search service corduroy` |
//!
//! Every value is kept under the service [`SERVICE`] and an item name: the name of the
//! agent file's `[secrets.<name>]` entry, so that every agent of a person's that names a
//! secret alike shares its value.
//!
//! - [`Keychain`] is the little a keychain is asked for: whether it is there, and to get,
//!   set and remove one item. [`Os`] is the system's own; [`Memory`] is one that forgets,
//!   for tests; [`Missing`] is a machine that has none.
//! - [`keychain_secret`] is one item as a [`Secret`]: the plugin an agent file's
//!   `[secrets.<name>]` entry of `kind = "keychain"` asks for.
//!
//! There is no fallback. A machine without a keychain (a server nobody is logged in to, a
//! container) says so, and the entry has to name another source. A value is never printed,
//! logged or put in an error.

#![expect(
    clippy::multiple_crate_versions,
    reason = "the Secret Service client's dependency tree pulls two versions of a macro crate"
)]

mod memory;
mod os;

use std::sync::{Arc, Mutex, PoisonError};

use corduroy_core::{Context, FnPlugin, PluginError, SECRETS, Scope, Secret, Secrets, plugin_fn};

pub use memory::{Memory, Missing};
pub use os::Os;

/// The service every item is kept under: what a person looks for in their keychain.
pub const SERVICE: &str = "corduroy";

/// The command a person is sent to when an item is not there.
const SET: &str = "corduroy secrets set";

/// A keychain: named values kept for one person, encrypted behind their login.
///
/// An item's name holds letters, digits, `-`, `_` and `.` ([`item_name`]). No error holds a
/// value.
pub trait Keychain: Send + Sync {
    /// What it is, as a person would call it: `the macOS Keychain`.
    fn name(&self) -> String;

    /// Whether it can be reached at all.
    ///
    /// # Errors
    ///
    /// Why not: this machine has none, or nobody is logged in to it.
    fn reachable(&self) -> Result<(), String>;

    /// The value kept as `item`, if there is one.
    ///
    /// # Errors
    ///
    /// If the keychain cannot be reached, is locked and stays so, or refuses.
    fn get(&self, item: &str) -> Result<Option<String>, String>;

    /// Keep `value` as `item`, in place of what was there.
    ///
    /// # Errors
    ///
    /// If the keychain cannot be reached or refuses.
    fn set(&self, item: &str, value: &str) -> Result<(), String>;

    /// Forget `item`. Returns whether it was there.
    ///
    /// # Errors
    ///
    /// If the keychain cannot be reached or refuses.
    fn remove(&self, item: &str) -> Result<bool, String>;
}

impl<K: Keychain + ?Sized> Keychain for Arc<K> {
    fn name(&self) -> String {
        (**self).name()
    }

    fn reachable(&self) -> Result<(), String> {
        (**self).reachable()
    }

    fn get(&self, item: &str) -> Result<Option<String>, String> {
        (**self).get(item)
    }

    fn set(&self, item: &str, value: &str) -> Result<(), String> {
        (**self).set(item, value)
    }

    fn remove(&self, item: &str) -> Result<bool, String> {
        (**self).remove(item)
    }
}

/// `item` if it can name an item: letters, digits, `-`, `_` and `.`, and at least one.
///
/// # Errors
///
/// Says what a name may hold.
pub fn item_name(item: &str) -> Result<&str, String> {
    let plain = |character: char| character.is_ascii_alphanumeric() || "-_.".contains(character);
    if !item.is_empty() && item.chars().all(plain) {
        Ok(item)
    } else {
        Err(format!(
            "`{item}` cannot name something in a keychain: a name holds letters, digits, `-`, \
             `_` and `.`"
        ))
    }
}

/// What a keychain keeps under one name, read once.
///
/// The keychain is asked again only until it has answered with a value, so a person is
/// asked to unlock it (or to let this program in) once for each run and not for each
/// request.
struct Kept {
    keychain: Arc<dyn Keychain>,
    item: String,
    secret: String,
    read: Mutex<Option<String>>,
    /// The hosts kept beside the value, once the keychain has been asked for them.
    hosts: Mutex<Option<Said>>,
}

/// What a keychain holds of the hosts a value may be sent to.
#[derive(Clone)]
enum Said {
    /// Nothing: the value was put in without any.
    Nothing,
    /// These.
    Hosts(Scope),
}

/// The name of the item that holds the hosts the value kept as `item` may be sent to: one
/// to a line, as an `http` grant writes them. A prototype (B9 in `docs/open-questions.md`).
#[must_use]
pub fn hosts_item(item: &str) -> String {
    format!("{item}.hosts")
}

/// The scope the lines of a hosts item say.
fn hosts_said(lines: &str) -> Scope {
    lines
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .fold(Scope::none(), Scope::allow)
}

impl Secret for Kept {
    fn reveal(&self) -> Result<String, String> {
        let mut read = self.read.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(value) = read.as_ref() {
            return Ok(value.clone());
        }
        let value = self.keychain.get(&self.item)?.ok_or_else(|| {
            format!(
                "{} holds nothing as `{}`: put the value there with `{SET} {}`",
                self.keychain.name(),
                self.item,
                self.secret
            )
        })?;
        *read = Some(value.clone());
        drop(read);
        Ok(value)
    }

    /// The hosts kept beside the value, said by the person who put it in. An item put in
    /// with none says nothing. A keychain that cannot be read names none: the value could
    /// not be had either.
    fn hosts(&self) -> Option<Scope> {
        let mut hosts = self.hosts.lock().unwrap_or_else(PoisonError::into_inner);
        let said = if let Some(said) = hosts.as_ref() {
            said.clone()
        } else {
            let Ok(lines) = self.keychain.get(&hosts_item(&self.item)) else {
                return Some(Scope::none());
            };
            let said = lines
                .as_deref()
                .map_or(Said::Nothing, |lines| Said::Hosts(hosts_said(lines)));
            *hosts = Some(said.clone());
            said
        };
        drop(hosts);
        match said {
            Said::Nothing => None,
            Said::Hosts(scope) => Some(scope),
        }
    }
}

/// The plugin that registers what `keychain` keeps as `item` as the secret `name`.
///
/// The keychain is not asked when the plugin is applied: an item that is missing, or a
/// machine without a keychain, shows when the secret is first revealed, with what to do.
pub fn keychain_secret(
    name: impl Into<String>,
    keychain: Arc<dyn Keychain>,
    item: impl Into<String>,
) -> FnPlugin<impl FnMut(&mut Context<'_>) -> Result<(), PluginError> + Send + Sync> {
    let (name, item) = (name.into(), item.into());
    plugin_fn(format!("secret-{name}"), &[SECRETS], move |ctx| {
        let secret: Arc<dyn Secret> = Arc::new(Kept {
            keychain: Arc::clone(&keychain),
            item: item.clone(),
            secret: name.clone(),
            read: Mutex::new(None),
            hosts: Mutex::new(None),
        });
        ctx.service::<Secrets>(SECRETS)?
            .register(ctx, name.clone(), secret)?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use corduroy_core::App;

    use super::*;

    #[test]
    fn a_name_holds_only_what_every_keychain_takes() {
        assert_eq!(
            item_name("work.anthropic_key-2"),
            Ok("work.anthropic_key-2")
        );
        for bad in ["", "a b", "a/b", "a#1", "é"] {
            assert!(item_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_item_is_a_secret_read_once_and_a_missing_one_says_what_to_do() {
        let keychain = Arc::new(Memory::new());
        let mut app = App::new();
        let secrets = Arc::new(Secrets::new());
        app.provide(SECRETS, Arc::clone(&secrets)).unwrap();
        let shared: Arc<dyn Keychain> = Arc::<Memory>::clone(&keychain);
        app.plugin(keychain_secret("key", shared, "key"));

        let missing = secrets.reveal("key").unwrap_err();
        assert_eq!(
            missing,
            "a keychain in memory holds nothing as `key`: put the value there with \
             `corduroy secrets set key`"
        );
        // Put there since, it is found: a miss is not remembered.
        keychain.set("key", "s3cret").unwrap();
        assert_eq!(secrets.reveal("key").unwrap(), "s3cret");
        // A value is read once for a run.
        keychain.set("key", "changed").unwrap();
        assert_eq!(secrets.reveal("key").unwrap(), "s3cret");
        assert_eq!(keychain.reads(), 2);
    }

    #[test]
    fn a_machine_without_a_keychain_says_so_and_falls_back_to_nothing() {
        let none: Arc<dyn Keychain> = Arc::new(Missing::new("nobody is logged in"));
        let mut app = App::new();
        let secrets = Arc::new(Secrets::new());
        app.provide(SECRETS, Arc::clone(&secrets)).unwrap();
        app.plugin(keychain_secret("key", none, "key"));
        assert_eq!(
            secrets.reveal("key").unwrap_err(),
            "this machine has no keychain (nobody is logged in)"
        );
    }
}
