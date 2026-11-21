//! Named collections that plugins contribute to.
//!
//! A [`Registry`] is the usual shape of a service: the host provides one under a well-known
//! name, plugins inject it and [`register`](Registry::register) entries, and each entry is
//! removed again when the plugin that added it is unloaded.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::error::Error;
use std::fmt;
use std::sync::{Arc, PoisonError, RwLock};

use crate::context::{Context, PluginError};

/// A name was registered twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Duplicate(pub String);

impl fmt::Display for Duplicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` is already registered", self.0)
    }
}

impl Error for Duplicate {}

impl From<Duplicate> for PluginError {
    fn from(error: Duplicate) -> Self {
        Self::new(error.to_string())
    }
}

/// A thread-safe map from names to shared entries.
pub struct Registry<T: ?Sized> {
    entries: RwLock<BTreeMap<String, Arc<T>>>,
}

impl<T: ?Sized> Default for Registry<T> {
    fn default() -> Self {
        Self {
            entries: RwLock::new(BTreeMap::new()),
        }
    }
}

impl<T: ?Sized> fmt::Debug for Registry<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Registry").field(&self.names()).finish()
    }
}

impl<T: ?Sized> Registry<T> {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an entry with no owner. Prefer [`register`](Self::register) inside plugins.
    ///
    /// # Errors
    ///
    /// Returns [`Duplicate`] if `name` is taken.
    pub fn insert(&self, name: impl Into<String>, entry: Arc<T>) -> Result<(), Duplicate> {
        match self
            .entries
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(name.into())
        {
            Entry::Occupied(taken) => Err(Duplicate(taken.key().clone())),
            Entry::Vacant(free) => {
                free.insert(entry);
                Ok(())
            }
        }
    }

    /// Remove an entry, returning it if it existed.
    pub fn remove(&self, name: &str) -> Option<Arc<T>> {
        self.entries
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name)
    }

    /// Look an entry up.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<T>> {
        self.entries
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .map(Arc::clone)
    }

    /// The registered names, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.entries
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .cloned()
            .collect()
    }

    /// Every entry, sorted by name.
    #[must_use]
    pub fn entries(&self) -> Vec<Arc<T>> {
        self.entries
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .map(Arc::clone)
            .collect()
    }
}

impl<T: ?Sized + Send + Sync + 'static> Registry<T> {
    /// Add an entry on behalf of the plugin behind `ctx`. The entry is removed when that
    /// plugin is unloaded.
    ///
    /// # Errors
    ///
    /// Returns [`Duplicate`] if `name` is taken.
    pub fn register(
        self: &Arc<Self>,
        ctx: &mut Context<'_>,
        name: impl Into<String>,
        entry: Arc<T>,
    ) -> Result<(), Duplicate> {
        let name = name.into();
        self.insert(name.clone(), entry)?;
        let registry = Arc::clone(self);
        ctx.effect(move || {
            registry.remove(&name);
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{App, plugin_fn};

    #[test]
    fn entries_are_named_sorted_and_unique() {
        let registry = Registry::<str>::new();
        registry.insert("b", Arc::from("second")).unwrap();
        registry.insert("a", Arc::from("first")).unwrap();

        assert_eq!(registry.names(), ["a", "b"]);
        assert_eq!(registry.get("a").as_deref(), Some("first"));
        assert_eq!(registry.entries().len(), 2);
        assert_eq!(
            registry
                .insert("a", Arc::from("again"))
                .unwrap_err()
                .to_string(),
            "`a` is already registered"
        );
        assert_eq!(registry.remove("a").as_deref(), Some("first"));
        assert!(registry.get("a").is_none());
    }

    #[test]
    fn registrations_follow_the_plugin_that_made_them() {
        let registry = Arc::new(Registry::<str>::new());
        let mut app = App::new();
        app.provide("names", Arc::clone(&registry)).unwrap();

        let id = app.plugin(plugin_fn("p", &["names"], |ctx| {
            let names = ctx.service::<Registry<str>>("names")?;
            names.register(ctx, "p/entry", Arc::from("value"))?;
            Ok(())
        }));

        assert_eq!(registry.names(), ["p/entry"]);
        app.dispose(id).unwrap();
        assert_eq!(registry.names(), Vec::<String>::new());
    }
}
