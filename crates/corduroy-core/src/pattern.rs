//! Glob patterns shared by capability scopes and event subscriptions.

use std::fmt;

/// A glob in which `*` matches any run of characters, including none.
///
/// There is no other syntax: every other character matches itself. `*` also matches `/`, so
/// `/work/*` covers the whole subtree below `/work`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pattern(String);

impl Pattern {
    /// Create a pattern from its textual form.
    #[must_use]
    pub fn new(pattern: impl Into<String>) -> Self {
        Self(pattern.into())
    }

    /// The pattern that matches everything.
    #[must_use]
    pub fn any() -> Self {
        Self::new("*")
    }

    /// The textual form this pattern was created from.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `value` matches this pattern.
    #[must_use]
    pub fn matches(&self, value: &str) -> bool {
        let mut parts = self.0.split('*');
        let Some(first) = parts.next() else {
            return value.is_empty();
        };
        let Some(mut rest) = value.strip_prefix(first) else {
            return false;
        };
        let Some(last) = parts.next_back() else {
            // No `*` at all: the pattern is a literal.
            return rest.is_empty();
        };
        for part in parts {
            let Some((_, after)) = rest.split_once(part) else {
                return false;
            };
            rest = after;
        }
        rest.ends_with(last)
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Pattern {
    fn from(pattern: &str) -> Self {
        Self::new(pattern)
    }
}

impl From<String> for Pattern {
    fn from(pattern: String) -> Self {
        Self::new(pattern)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, value: &str) -> bool {
        Pattern::new(pattern).matches(value)
    }

    #[test]
    fn literals_match_only_themselves() {
        assert!(matches("tool/call", "tool/call"));
        assert!(!matches("tool/call", "tool/called"));
        assert!(!matches("tool/call", "tool"));
        assert!(matches("", ""));
        assert!(!matches("", "x"));
    }

    #[test]
    fn star_matches_any_run_including_separators() {
        assert!(matches("*", ""));
        assert!(matches("*", "anything/at/all"));
        assert!(matches("/work/*", "/work/a/b.txt"));
        assert!(!matches("/work/*", "/workshop/a"));
        assert!(matches("*.md", "notes/SKILL.md"));
        assert!(!matches("*.md", "notes/SKILL.mdx"));
    }

    #[test]
    fn several_stars_match_in_order() {
        assert!(matches("a*b*c", "a-b-c"));
        assert!(matches("a*b*c", "abc"));
        assert!(!matches("a*b*c", "a-c-b"));
        assert!(matches("*tool*", "a/tool/b"));
        // The suffix may not reuse text the middle already consumed.
        assert!(!matches("a*bc*c", "abc"));
    }
}
