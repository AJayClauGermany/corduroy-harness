//! Keychains that are not the system's: one that forgets, and one that is not there.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::{Keychain, item_name};

/// A keychain kept in memory, which forgets when it is dropped: for tests, and for trying
/// a command out where there is no keychain. Nothing in it is encrypted.
#[derive(Debug, Default)]
pub struct Memory {
    items: Mutex<BTreeMap<String, String>>,
    reads: AtomicU32,
}

impl Memory {
    /// An empty one.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How often an item was asked for.
    #[must_use]
    pub fn reads(&self) -> u32 {
        self.reads.load(Ordering::Relaxed)
    }

    /// The names of the items kept, sorted.
    #[must_use]
    pub fn items(&self) -> Vec<String> {
        self.kept().keys().cloned().collect()
    }

    fn kept(&self) -> MutexGuard<'_, BTreeMap<String, String>> {
        self.items.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Keychain for Memory {
    fn name(&self) -> String {
        "a keychain in memory".to_owned()
    }

    fn reachable(&self) -> Result<(), String> {
        Ok(())
    }

    fn get(&self, item: &str) -> Result<Option<String>, String> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(self.kept().get(item_name(item)?).cloned())
    }

    fn set(&self, item: &str, value: &str) -> Result<(), String> {
        self.kept()
            .insert(item_name(item)?.to_owned(), value.to_owned());
        Ok(())
    }

    fn remove(&self, item: &str) -> Result<bool, String> {
        Ok(self.kept().remove(item_name(item)?).is_some())
    }
}

/// The keychain of a machine that has none: everything asked of it fails, saying why.
#[derive(Debug, Clone)]
pub struct Missing {
    why: String,
}

impl Missing {
    /// One that is not there because of `why`.
    #[must_use]
    pub fn new(why: impl Into<String>) -> Self {
        Self { why: why.into() }
    }

    fn refusal(&self) -> String {
        format!("this machine has no keychain ({})", self.why)
    }
}

impl Keychain for Missing {
    fn name(&self) -> String {
        "no keychain".to_owned()
    }

    fn reachable(&self) -> Result<(), String> {
        Err(self.refusal())
    }

    fn get(&self, _item: &str) -> Result<Option<String>, String> {
        Err(self.refusal())
    }

    fn set(&self, _item: &str, _value: &str) -> Result<(), String> {
        Err(self.refusal())
    }

    fn remove(&self, _item: &str) -> Result<bool, String> {
        Err(self.refusal())
    }
}
