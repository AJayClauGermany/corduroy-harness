//! What a plugin is allowed to do.
//!
//! A *capability* is the name of something the host can do on a plugin's behalf, such as
//! `fs-read` or `kernel`. A plugin starts with none. The host hands each plugin a set of
//! [`Grants`]: the capabilities it may use, each narrowed by a [`Scope`] that says which
//! resources (paths, kernel names, topics) the capability reaches.
//!
//! The model follows Deno's permissions and Tauri's capabilities: nothing is ambient, a grant
//! names both the ability and its reach, and a `deny` entry always beats an `allow` entry.

use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::error::Error;
use core::fmt;

use crate::pattern::Pattern;

/// Which resources a granted capability reaches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    allow: Vec<Pattern>,
    deny: Vec<Pattern>,
    /// Scopes this one is narrowed to: a resource must be inside each of them as well.
    within: Vec<Self>,
}

impl Scope {
    /// A scope that reaches nothing until [`allow`](Self::allow) is called.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// A scope that reaches every resource.
    #[must_use]
    pub fn any() -> Self {
        Self::none().allow(Pattern::any())
    }

    /// A scope that reaches `root` and everything below it, for `/`-separated resources.
    #[must_use]
    pub fn subtree(root: &str) -> Self {
        let root = root.trim_end_matches('/');
        Self::none().allow(root).allow(format!("{root}/*"))
    }

    /// Also reach resources matching `pattern`.
    #[must_use]
    pub fn allow(mut self, pattern: impl Into<Pattern>) -> Self {
        self.allow.push(pattern.into());
        self
    }

    /// Never reach resources matching `pattern`, whatever is allowed.
    #[must_use]
    pub fn deny(mut self, pattern: impl Into<Pattern>) -> Self {
        self.deny.push(pattern.into());
        self
    }

    /// Narrow this scope to what `outer` also reaches: the result never reaches further
    /// than either. This is how a plugin passes on part of a capability without passing on
    /// more than it holds.
    #[must_use]
    pub fn within(mut self, outer: &Self) -> Self {
        self.within.push(outer.clone());
        self
    }

    /// Whether this scope reaches nothing at all: it allows no pattern.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.allow.is_empty()
    }

    /// The patterns this scope allows, before any narrowing.
    pub fn allowed(&self) -> impl Iterator<Item = &str> {
        self.allow.iter().map(Pattern::as_str)
    }

    /// The patterns this scope denies, before any narrowing.
    pub fn denied(&self) -> impl Iterator<Item = &str> {
        self.deny.iter().map(Pattern::as_str)
    }

    /// Whether `resource` is inside this scope.
    #[must_use]
    pub fn permits(&self, resource: &str) -> bool {
        self.allow.iter().any(|pattern| pattern.matches(resource))
            && !self.deny.iter().any(|pattern| pattern.matches(resource))
            && self.within.iter().all(|outer| outer.permits(resource))
    }
}

/// The capabilities one plugin has been given.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grants {
    scopes: BTreeMap<String, Scope>,
}

impl Grants {
    /// No capabilities at all: the starting point for every plugin.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Grant `capability` within `scope`, replacing any earlier grant of the same capability.
    #[must_use]
    pub fn grant(mut self, capability: impl Into<String>, scope: Scope) -> Self {
        self.scopes.insert(capability.into(), scope);
        self
    }

    /// The scope `capability` was granted in, if it was granted.
    #[must_use]
    pub fn scope(&self, capability: &str) -> Option<&Scope> {
        self.scopes.get(capability)
    }

    /// Whether `capability` was granted, in any scope.
    #[must_use]
    pub fn has(&self, capability: &str) -> bool {
        self.scopes.contains_key(capability)
    }

    /// Names of the granted capabilities, sorted.
    pub fn capabilities(&self) -> impl Iterator<Item = &str> {
        self.scopes.keys().map(String::as_str)
    }

    /// Check that `capability` was granted and that its scope reaches `resource`.
    ///
    /// # Errors
    ///
    /// Returns [`Denied`] naming the capability and resource if either check fails.
    pub fn check(&self, capability: &str, resource: &str) -> Result<(), Denied> {
        let permitted = self
            .scopes
            .get(capability)
            .is_some_and(|scope| scope.permits(resource));
        if permitted {
            Ok(())
        } else {
            Err(Denied {
                capability: capability.to_owned(),
                resource: resource.to_owned(),
            })
        }
    }
}

/// A capability check that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denied {
    /// The capability that was needed.
    pub capability: String,
    /// The resource the plugin tried to reach.
    pub resource: String,
}

impl fmt::Display for Denied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "capability `{}` does not cover `{}`",
            self.capability, self.resource
        )
    }
}

impl Error for Denied {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn nothing_is_granted_by_default() {
        let grants = Grants::none();

        assert!(!grants.has("fs-read"));
        assert!(grants.check("fs-read", "/etc/passwd").is_err());
    }

    #[test]
    fn a_grant_reaches_only_its_scope() {
        let grants = Grants::none().grant("fs-read", Scope::subtree("/work/"));

        assert!(grants.check("fs-read", "/work").is_ok());
        assert!(grants.check("fs-read", "/work/src/lib.rs").is_ok());
        assert!(grants.check("fs-read", "/workshop/x").is_err());
        assert!(grants.check("fs-write", "/work/src/lib.rs").is_err());
    }

    #[test]
    fn a_scope_within_another_reaches_only_what_both_reach() {
        let outer = Scope::subtree("/work").deny("*.env");
        let wide = Scope::any().within(&outer);
        let narrow = Scope::subtree("/work/out").within(&outer);

        assert!(wide.permits("/work/a.rs"));
        assert!(!wide.permits("/etc/passwd"));
        assert!(!wide.permits("/work/.env"));
        assert!(narrow.permits("/work/out/b"));
        assert!(!narrow.permits("/work/a.rs"));
        assert!(Scope::none().is_empty() && !Scope::any().is_empty());
        assert_eq!(outer.denied().collect::<Vec<_>>(), ["*.env"]);
        assert_eq!(
            Scope::subtree("/w").allowed().collect::<Vec<_>>(),
            ["/w", "/w/*"]
        );
    }

    #[test]
    fn deny_beats_allow() {
        let scope = Scope::any().deny("*.env");

        assert!(scope.permits("/work/readme.md"));
        assert!(!scope.permits("/work/.env"));
        assert!(!Scope::none().permits("anything"));
    }

    #[test]
    fn denial_names_capability_and_resource() {
        let denied = Grants::none().check("kernel", "shell").unwrap_err();

        assert_eq!(
            denied.to_string(),
            "capability `kernel` does not cover `shell`"
        );
        assert_eq!(
            Grants::none()
                .grant("b", Scope::any())
                .grant("a", Scope::any())
                .capabilities()
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
    }
}
