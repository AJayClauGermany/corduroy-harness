//! Which versions of the plugin world this host loads.
//!
//! The world's version is the WIT package's, and it reads as a semantic version does: a
//! minor version adds an import interface, or a function inside an interface's grant, and
//! anything else is a major one. A host checks what an add-on was built for when it loads it,
//! before anything is instantiated, so that an add-on from another world is refused in the
//! host's own words and not by the engine when it is first started.

use std::fmt;

use super::{PACKAGE, WORLD};

/// A version of the plugin world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

/// The version of the world this host is built against: the one `wit/corduroy.wit` names.
pub(super) const HOST: Version = Version::new(0, 1, 0);

impl Version {
    pub(super) const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// The version `text` writes, as `1.2.3`.
    fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.').map(str::parse::<u64>);
        let version = Self::new(
            parts.next()?.ok()?,
            parts.next()?.ok()?,
            parts.next()?.ok()?,
        );
        parts.next().is_none().then_some(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// What a host does with an add-on built for some version of the world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Admits {
    /// It loads: everything it could ask for is here.
    Yes,
    /// It was built for a later version of this host's major version, so it loads if every
    /// interface and function it imports is here, and is refused if one is not.
    IfComplete,
    /// It does not load, and why.
    No(&'static str),
}

/// What a host whose world is `host` does with an add-on built for `built`.
///
/// An add-on built for an earlier version of the host's major version always loads. One
/// built for a later one loads if the host has all it asks for. Another major version is
/// refused, and before 1.0 a host promises its own version only: there a minor version is
/// what a major one is afterwards.
pub(super) const fn admits(host: Version, built: Version) -> Admits {
    if host.major != built.major {
        return Admits::No("a host loads add-ons built for its own major version of the world");
    }
    if host.major == 0 {
        return if host.minor == built.minor {
            Admits::Yes
        } else {
            Admits::No("before 1.0 a host loads only add-ons built for its own version")
        };
    }
    if built.minor <= host.minor {
        Admits::Yes
    } else {
        Admits::IfComplete
    }
}

/// The host interface an import names and the version of the world it was built against, if
/// the import is one of the plugin package's.
///
/// `corduroy:plugin/fs-read@0.1.0` names `fs-read` at 0.1.0.
pub(super) fn import(name: &str) -> Option<(&str, Option<Version>)> {
    let interface = name.strip_prefix(PACKAGE)?;
    Some(match interface.split_once('@') {
        Some((interface, version)) => (interface, Version::parse(version)),
        None => (interface, None),
    })
}

/// Why an add-on built for `built`, which imports the interface `unknown` this host does
/// not have (if it imports one), is not loaded; `None` if nothing stands in its way so far.
pub(super) fn refusal(admits: Admits, built: Version, unknown: Option<&str>) -> Option<String> {
    let package = PACKAGE.trim_end_matches('/');
    match (admits, unknown) {
        (Admits::No(why), _) => Some(format!(
            "it was built for the plugin world {package}@{built} and this host's is \
             {WORLD}: {why}. Build it again, against this host's world"
        )),
        (Admits::IfComplete, Some(interface)) => Some(format!(
            "it was built for the plugin world {package}@{built} and imports `{interface}`, \
             which this host's, {WORLD}, does not have"
        )),
        (Admits::Yes, Some(interface)) => Some(format!(
            "it imports `{PACKAGE}{interface}`, which no capability provides"
        )),
        (Admits::Yes | Admits::IfComplete, None) => None,
    }
}

/// Why an add-on built for the later version `built` is not loaded, when the engine found
/// `missing` among what it imports.
pub(super) fn incomplete(built: Version, missing: &str) -> String {
    let package = PACKAGE.trim_end_matches('/');
    format!(
        "it was built for the plugin world {package}@{built} and asks for what this host's, \
         {WORLD}, does not have: {missing}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn v(major: u64, minor: u64, patch: u64) -> Version {
        Version::new(major, minor, patch)
    }

    #[test]
    fn this_host_s_version_is_the_one_the_world_names() {
        assert_eq!(WORLD, format!("{}@{HOST}", PACKAGE.trim_end_matches('/')));
    }

    #[test]
    fn an_import_names_its_interface_and_the_world_it_was_built_against() {
        assert_eq!(
            import("corduroy:plugin/fs-read@0.1.0"),
            Some(("fs-read", Some(v(0, 1, 0))))
        );
        assert_eq!(import("corduroy:plugin/types"), Some(("types", None)));
        assert_eq!(
            import("corduroy:plugin/llm@1.x"),
            Some(("llm", None)),
            "not a version"
        );
        assert_eq!(import("wasi:io/streams@0.2.0"), None);
    }

    #[test]
    fn before_1_0_a_host_loads_its_own_version_only() {
        assert_eq!(admits(v(0, 1, 0), v(0, 1, 0)), Admits::Yes);
        // A patch changes wording and no shape.
        assert_eq!(admits(v(0, 1, 0), v(0, 1, 3)), Admits::Yes);
        assert!(matches!(admits(v(0, 1, 0), v(0, 2, 0)), Admits::No(_)));
        assert!(matches!(admits(v(0, 2, 0), v(0, 1, 0)), Admits::No(_)));
        assert!(matches!(admits(v(0, 1, 0), v(1, 0, 0)), Admits::No(_)));
    }

    #[test]
    fn from_1_0_an_earlier_add_on_always_loads_and_a_later_one_if_all_it_asks_for_is_here() {
        let host = v(1, 2, 0);
        assert_eq!(admits(host, v(1, 0, 0)), Admits::Yes);
        assert_eq!(admits(host, v(1, 2, 5)), Admits::Yes);
        assert_eq!(admits(host, v(1, 3, 0)), Admits::IfComplete);
        assert!(matches!(admits(host, v(2, 0, 0)), Admits::No(_)));
        assert!(matches!(admits(host, v(0, 9, 0)), Admits::No(_)));
    }

    #[test]
    fn a_refusal_names_the_version_built_for_this_host_s_and_what_is_missing() {
        let later = v(1, 3, 0);
        let refused = refusal(Admits::IfComplete, later, Some("progress")).unwrap();
        assert!(refused.contains("corduroy:plugin@1.3.0"), "{refused}");
        assert!(refused.contains("imports `progress`"), "{refused}");
        assert!(refused.contains(WORLD), "{refused}");
        assert_eq!(refusal(Admits::IfComplete, later, None), None);

        let missing = incomplete(later, "function `read-bytes`");
        assert!(missing.contains("corduroy:plugin@1.3.0"), "{missing}");
        assert!(missing.ends_with("function `read-bytes`"), "{missing}");

        let other = refusal(Admits::No("why"), v(2, 0, 0), None).unwrap();
        assert!(other.contains("corduroy:plugin@2.0.0"), "{other}");
        assert!(other.contains(WORLD), "{other}");
        assert_eq!(refusal(Admits::Yes, HOST, None), None);
    }
}
