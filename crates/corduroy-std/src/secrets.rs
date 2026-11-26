//! What a person does about a secret besides naming it: put its value in, and see how it
//! stands.
//!
//! A secret read from the environment is set wherever the person sets their variables. One
//! kept in the system's keychain has to be put there, by the person, with a command that
//! does not show it: a [`Keep`] is how the command does that for one kind of secret.
//! Whatever its kind, a secret can say how it stands without giving its value away (the
//! variable is set, the item is there, somebody is signed in), which is what the command
//! lists. The [`Standard`] holds both by kind, so a command of one's own can bring others.
//!
//! [`Standard`]: crate::Standard

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use corduroy_keychain::Keychain;

use crate::extension::{Asked, chatgpt_in, env_variable, keychain_item};

/// How a person puts in, and takes out, the value of one kind of secret.
pub trait Keep: Send + Sync {
    /// Keep `value` as the value of the entry `asked`. Returns where it is kept now, as a
    /// person would call the place.
    ///
    /// # Errors
    ///
    /// Says why it is not kept, without the value.
    fn put(&self, asked: &Asked<'_>, value: &str) -> Result<String, String>;

    /// Forget the value of the entry `asked`. Returns whether there was one.
    ///
    /// # Errors
    ///
    /// Says why it is still there.
    fn remove(&self, asked: &Asked<'_>) -> Result<bool, String>;
}

/// Keeping the value of a `[secrets.<name>]` entry of the kind
/// [`KEYCHAIN_SECRET`](crate::KEYCHAIN_SECRET) in a keychain.
#[derive(Clone)]
pub struct InKeychain {
    keychain: Arc<dyn Keychain>,
}

impl std::fmt::Debug for InKeychain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InKeychain")
            .field("keychain", &self.keychain.name())
            .finish()
    }
}

impl InKeychain {
    /// Keeping in `keychain`.
    #[must_use]
    pub const fn new(keychain: Arc<dyn Keychain>) -> Self {
        Self { keychain }
    }
}

impl Keep for InKeychain {
    fn put(&self, asked: &Asked<'_>, value: &str) -> Result<String, String> {
        self.keychain.set(keychain_item(asked)?, value)?;
        Ok(self.keychain.name())
    }

    fn remove(&self, asked: &Asked<'_>) -> Result<bool, String> {
        self.keychain.remove(keychain_item(asked)?)
    }
}

/// How a secret stands: what there is to say when its value can be had, or why it cannot
/// and what the person can do. Neither holds the value.
pub type State = Result<String, String>;

/// Says how the secret of one entry stands.
pub type Stand = dyn Fn(&Asked<'_>) -> State + Send + Sync;

/// How a secret read from the environment stands: whether its variable is set.
///
/// # Errors
///
/// As a [`State`]: the variable is not set, or the entry is wrong.
pub fn env_state(asked: &Asked<'_>) -> State {
    let variable = env_variable(asked)?;
    match std::env::var_os(variable) {
        Some(value) if !value.is_empty() => Ok(format!("${variable} is set")),
        Some(_) => Err(format!("${variable} is empty")),
        None => Err(format!("${variable} is not set")),
    }
}

/// How a secret kept in `keychain` stands: whether its item is there.
///
/// # Errors
///
/// As a [`State`]: nothing is kept, or the keychain cannot be reached.
pub fn keychain_state(asked: &Asked<'_>, keychain: &dyn Keychain) -> State {
    let item = keychain_item(asked)?;
    match keychain.get(item)? {
        Some(_) => Ok(format!("in {}", keychain.name())),
        None => Err(format!(
            "{} holds nothing as `{item}`: put the value there with `corduroy secrets set {}`",
            keychain.name(),
            asked.name
        )),
    }
}

/// How a `ChatGPT` sign-in stands: who is signed in, where it is kept and how long its
/// token is good for.
///
/// Nothing is asked of `OpenAI`. A sign-in still in the file it was kept in before the
/// keychain was where one goes is said to be there, with what moves it.
///
/// # Errors
///
/// As a [`State`]: nobody is signed in, the plan may not be used, or what is kept cannot
/// be read.
pub fn chatgpt_state(asked: &Asked<'_>, keychain: &Arc<dyn Keychain>) -> State {
    let chatgpt = chatgpt_in(asked, keychain)?;
    let store = chatgpt.store();
    let Some(record) = store.load()? else {
        // Looked for only where a file is not what this entry reads.
        let left = store.file().is_file() && store.place() != store.file().display().to_string();
        return Err(if left {
            format!(
                "the sign-in is still in {}, and this entry keeps it in {}: `corduroy login {}` \
                 moves it, without the browser",
                store.file().display(),
                store.place(),
                asked.name
            )
        } else {
            format!(
                "nobody is signed in to ChatGPT (looked in {}): run `corduroy login {}`",
                store.place(),
                asked.name
            )
        });
    };
    let who = record
        .email
        .as_deref()
        .unwrap_or("an account without an email");
    if !record.plan() {
        return Err(format!(
            "signed in as {who}, without the use of the plan: run `corduroy login {}` and \
             allow it",
            asked.name
        ));
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let token = match record.expires_at().checked_sub(now) {
        Some(left) if left > 0 => format!("its token is good for {} more minutes", left / 60),
        _ => "its token has run out and is renewed at the next request".to_owned(),
    };
    Ok(format!(
        "signed in as {who}, kept in {}; {token}",
        store.place()
    ))
}

#[cfg(test)]
mod tests {
    use corduroy_keychain::{Memory, Missing};
    use serde_json::{Value, json};

    use super::*;
    use crate::places::Places;

    fn asked<'a>(name: &'a str, config: &'a Value, places: &'a Places) -> Asked<'a> {
        Asked {
            name,
            config,
            places,
        }
    }

    #[test]
    fn a_value_is_put_in_the_keychain_seen_there_and_taken_out() {
        let places = Places::new("/work".into(), None);
        let none = json!({});
        let entry = asked("anthropic", &none, &places);
        let keychain = Arc::new(Memory::new());
        let keep = InKeychain::new(Arc::<Memory>::clone(&keychain));

        let missing = keychain_state(&entry, &*keychain).unwrap_err();
        assert_eq!(
            missing,
            "a keychain in memory holds nothing as `anthropic`: put the value there with \
             `corduroy secrets set anthropic`"
        );
        assert_eq!(keep.put(&entry, "sk-1").unwrap(), "a keychain in memory");
        assert_eq!(keychain.items(), ["anthropic"]);
        assert_eq!(
            keychain_state(&entry, &*keychain).unwrap(),
            "in a keychain in memory"
        );
        assert!(keep.remove(&entry).unwrap());
        assert!(!keep.remove(&entry).unwrap());

        // The kind takes no setting, and a name that could be another kind's item is refused.
        let odd = json!({"item": "other"});
        assert!(keep.put(&asked("anthropic", &odd, &places), "v").is_err());
        let dotted = keep.put(&asked("chatgpt-sign-in.plan", &none, &places), "v");
        assert!(dotted.unwrap_err().contains("only letters, digits"));
        assert_eq!(keychain.items(), Vec::<String>::new());
    }

    #[test]
    fn a_machine_without_a_keychain_keeps_nothing_and_says_so() {
        let places = Places::new("/work".into(), None);
        let none = json!({});
        let entry = asked("anthropic", &none, &places);
        let missing = Missing::new("nobody is logged in");
        let said = "this machine has no keychain (nobody is logged in)";
        assert_eq!(keychain_state(&entry, &missing).unwrap_err(), said);
        let keep = InKeychain::new(Arc::new(missing));
        assert_eq!(keep.put(&entry, "sk-1").unwrap_err(), said);
    }

    #[test]
    fn a_secret_from_the_environment_says_whether_its_variable_is_set() {
        let places = Places::new("/work".into(), None);
        let path = json!({"env": "PATH"});
        assert_eq!(
            env_state(&asked("p", &path, &places)).unwrap(),
            "$PATH is set"
        );
        let unset = json!({"env": "CORDUROY_NEVER_SET"});
        assert_eq!(
            env_state(&asked("p", &unset, &places)).unwrap_err(),
            "$CORDUROY_NEVER_SET is not set"
        );
        assert!(env_state(&asked("p", &json!({}), &places)).is_err());
    }
}
