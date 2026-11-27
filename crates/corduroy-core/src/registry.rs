//! Named collections that plugins contribute to.
//!
//! A [`Registry`] is the usual shape of a service: the host provides one under a well-known
//! name, plugins inject it and [`register`](Registry::register) entries, and each entry is
//! removed again when the plugin that added it is unloaded.

use alloc::collections::BTreeMap;
use alloc::collections::btree_map::Entry;
use alloc::string::String;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::error::Error;
use core::fmt;

use crate::sync::Lock;

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

/// Why an entry could not be put in another's place.
///
/// More reasons may follow, so code that reads one needs an arm for the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NotReplaced {
    /// Nothing is registered under the name, so there is nothing to stand in for.
    Missing(String),
    /// The entry is not the same as the one it would stand in for: the name, and how the
    /// two differ.
    Differs(String, String),
}

impl fmt::Display for NotReplaced {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(name) => write!(
                f,
                "`{name}` is not registered, so it cannot be replaced: what offers it comes \
                 first"
            ),
            Self::Differs(name, how) => {
                write!(f, "`{name}` cannot be replaced by this one: {how}")
            }
        }
    }
}

impl Error for NotReplaced {}

impl From<NotReplaced> for PluginError {
    fn from(error: NotReplaced) -> Self {
        Self::new(error.to_string())
    }
}

/// What is registered under each name.
type Names<T> = BTreeMap<String, Vec<Held<T>>>;

/// One entry under a name, and whether it is there in another's place.
struct Held<T: ?Sized> {
    entry: Arc<T>,
    /// Registered with [`Registry::replace`], over the entry it stands in for.
    stands_in: bool,
}

/// A thread-safe map from names to shared entries.
///
/// A name has one entry in use. An entry may be put in another's place
/// ([`replace`](Self::replace)): the one it stands in for stays registered beneath it,
/// unused, and is in use again once the stand-in is withdrawn.
pub struct Registry<T: ?Sized> {
    /// For each name, what is registered under it: the last is the one in use, and those
    /// before it are the entries it stands in for. Never empty.
    entries: Lock<Names<T>>,
}

impl<T: ?Sized> Default for Registry<T> {
    fn default() -> Self {
        Self {
            entries: Lock::new(BTreeMap::new()),
        }
    }
}

impl<T: ?Sized> fmt::Debug for Registry<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Registry").field(&self.names()).finish()
    }
}

/// The entry in use among `held`.
fn used<T: ?Sized>(held: &[Held<T>]) -> Option<Arc<T>> {
    held.last().map(|held| Arc::clone(&held.entry))
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
    /// Returns [`Duplicate`] if `name` is taken. A name held only by an entry that stands
    /// in for another is not: the entry goes beneath the stand-in, which stays in use, as
    /// when what it replaced is registered again after going away.
    pub fn insert(&self, name: impl Into<String>, entry: Arc<T>) -> Result<(), Duplicate> {
        let entry = Held {
            entry,
            stands_in: false,
        };
        self.entries
            .with(|entries| match entries.entry(name.into()) {
                Entry::Occupied(mut taken) => {
                    if taken.get().iter().all(|held| held.stands_in) {
                        taken.get_mut().insert(0, entry);
                        Ok(())
                    } else {
                        Err(Duplicate(taken.key().clone()))
                    }
                }
                Entry::Vacant(free) => {
                    free.insert(Vec::from([entry]));
                    Ok(())
                }
            })
    }

    /// Remove whatever is registered under `name`, returning the entry that was in use if
    /// there was one.
    pub fn remove(&self, name: &str) -> Option<Arc<T>> {
        self.entries
            .with(|entries| entries.remove(name).as_deref().and_then(used))
    }

    /// Take `entry` out from under `name`, wherever it is there: in use, or beneath what
    /// stands in for it. Whatever else is registered under the name stays, and the last
    /// of it is in use.
    fn withdraw(&self, name: &str, entry: &Arc<T>) {
        self.entries.with(|entries| {
            let Some(held) = entries.get_mut(name) else {
                return;
            };
            held.retain(|held| !core::ptr::addr_eq(Arc::as_ptr(&held.entry), Arc::as_ptr(entry)));
            if held.is_empty() {
                entries.remove(name);
            }
        });
    }

    /// Look an entry up: the one in use under `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<T>> {
        self.entries
            .with(|entries| entries.get(name).map(Vec::as_slice).and_then(used))
    }

    /// The registered names, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.entries
            .with(|entries| entries.keys().cloned().collect())
    }

    /// Every entry in use, sorted by name.
    #[must_use]
    pub fn entries(&self) -> Vec<Arc<T>> {
        self.entries
            .with(|entries| entries.values().filter_map(|held| used(held)).collect())
    }

    /// Whether the entry in use under `name` is there in another's place.
    #[must_use]
    pub fn replaced(&self, name: &str) -> bool {
        self.entries.with(|entries| {
            entries
                .get(name)
                .and_then(|held| held.last())
                .is_some_and(|held| held.stands_in)
        })
    }
}

impl<T: ?Sized + Send + Sync + 'static> Registry<T> {
    /// Add an entry on behalf of the plugin behind `ctx`. The entry is removed when that
    /// plugin is unloaded.
    ///
    /// # Errors
    ///
    /// Returns [`Duplicate`] if `name` is taken (see [`insert`](Self::insert)).
    pub fn register(
        self: &Arc<Self>,
        ctx: &mut Context<'_>,
        name: impl Into<String>,
        entry: Arc<T>,
    ) -> Result<(), Duplicate> {
        let name = name.into();
        self.insert(name.clone(), Arc::clone(&entry))?;
        let registry = Arc::clone(self);
        ctx.effect(move || registry.withdraw(&name, &entry));
        Ok(())
    }

    /// Put `entry` in the place of the one in use under `name`, on behalf of the plugin
    /// behind `ctx`, if `same` finds nothing to tell the two apart. `same` is handed the
    /// entry in use and says how the new one differs, or that it does not.
    ///
    /// The entry it stands in for stays registered and unused. It is in use again when
    /// the plugin behind `ctx` is unloaded; and if what registered it is unloaded first,
    /// the stand-in stays.
    ///
    /// # Errors
    ///
    /// Returns [`NotReplaced::Missing`] if nothing is registered under `name`, and
    /// [`NotReplaced::Differs`] with what `same` said if the two differ.
    pub fn replace(
        self: &Arc<Self>,
        ctx: &mut Context<'_>,
        name: impl Into<String>,
        entry: Arc<T>,
        same: impl FnOnce(&T) -> Result<(), String>,
    ) -> Result<(), NotReplaced> {
        let name = name.into();
        self.entries.with(|entries| {
            let held = entries
                .get_mut(&name)
                .ok_or_else(|| NotReplaced::Missing(name.clone()))?;
            let in_use = held
                .last()
                .ok_or_else(|| NotReplaced::Missing(name.clone()))?;
            same(&in_use.entry).map_err(|how| NotReplaced::Differs(name.clone(), how))?;
            held.push(Held {
                entry: Arc::clone(&entry),
                stands_in: true,
            });
            Ok(())
        })?;
        let registry = Arc::clone(self);
        ctx.effect(move || registry.withdraw(&name, &entry));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloc::format;

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

    #[test]
    fn an_entry_stands_in_for_another_only_if_they_are_the_same() {
        let registry = Arc::new(Registry::<str>::new());
        let mut app = App::new();
        app.provide("names", Arc::clone(&registry)).unwrap();
        let first = app.plugin(plugin_fn("first", &["names"], |ctx| {
            let names = ctx.service::<Registry<str>>("names")?;
            names.register(ctx, "greeting", Arc::from("hello"))?;
            Ok(())
        }));

        // One that differs is refused, with how, and nothing changes.
        app.plugin(plugin_fn("other", &["names"], |ctx| {
            let names = ctx.service::<Registry<str>>("names")?;
            let refused = names
                .replace(ctx, "greeting", Arc::from("goodbye"), |used| {
                    Err(format!("it is not `{used}`"))
                })
                .unwrap_err();
            assert_eq!(
                refused.to_string(),
                "`greeting` cannot be replaced by this one: it is not `hello`"
            );
            let missing = names
                .replace(ctx, "farewell", Arc::from("bye"), |_| Ok(()))
                .unwrap_err();
            assert!(matches!(missing, NotReplaced::Missing(name) if name == "farewell"));
            Ok(())
        }));
        assert_eq!(registry.get("greeting").as_deref(), Some("hello"));
        assert!(!registry.replaced("greeting"));

        // One that is the same takes the name, and gives it back when it goes.
        let second = app.plugin(plugin_fn("second", &["names"], |ctx| {
            let names = ctx.service::<Registry<str>>("names")?;
            names.replace(ctx, "greeting", Arc::from("hello again"), |_| Ok(()))?;
            Ok(())
        }));
        assert_eq!(registry.get("greeting").as_deref(), Some("hello again"));
        assert!(registry.replaced("greeting"));
        assert_eq!(registry.names(), ["greeting"]);
        assert_eq!(registry.entries().len(), 1);
        app.dispose(second).unwrap();
        assert_eq!(registry.get("greeting").as_deref(), Some("hello"));
        assert!(!registry.replaced("greeting"));
        app.dispose(first).unwrap();
        assert_eq!(registry.names(), Vec::<String>::new());
    }

    #[test]
    fn a_stand_in_outlives_what_it_replaced_and_lets_it_back_beneath() {
        let registry = Arc::new(Registry::<str>::new());
        let mut app = App::new();
        app.provide("names", Arc::clone(&registry)).unwrap();
        let original = || {
            plugin_fn("first", &["names"], |ctx| {
                let names = ctx.service::<Registry<str>>("names")?;
                names.register(ctx, "greeting", Arc::from("hello"))?;
                Ok(())
            })
        };
        let first = app.plugin(original());
        let second = app.plugin(plugin_fn("second", &["names"], |ctx| {
            let names = ctx.service::<Registry<str>>("names")?;
            names.replace(ctx, "greeting", Arc::from("hello again"), |_| Ok(()))?;
            Ok(())
        }));

        // What was replaced goes: the stand-in is still what the name gives.
        app.dispose(first).unwrap();
        assert_eq!(registry.get("greeting").as_deref(), Some("hello again"));
        // It comes back beneath the stand-in, and is in use once the stand-in goes.
        app.plugin(original());
        assert_eq!(registry.get("greeting").as_deref(), Some("hello again"));
        app.dispose(second).unwrap();
        assert_eq!(registry.get("greeting").as_deref(), Some("hello"));
        // A second entry that stands in for nothing is still a duplicate.
        assert!(registry.insert("greeting", Arc::from("again")).is_err());
    }
}
