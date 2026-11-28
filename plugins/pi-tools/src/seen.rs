//! What each file held when these tools last read or wrote it, so that `write` and `edit`
//! can refuse a file that something else has changed since.
//!
//! It is kept in the instance's memory: one record for every session the instance serves,
//! gone when the instance is. So it catches a shell command, another add-on and the
//! person's own editor, and forgets rather than refuses when in doubt. Two sessions served
//! by one instance share the record, which is every session today.

use std::collections::BTreeMap;

/// Files remembered at most. One more and all are forgotten: forgetting only means a
/// change goes unnoticed, as it did before.
const MAX_FILES: usize = 4096;

/// What files held, by path.
#[derive(Default)]
pub struct Seen {
    files: BTreeMap<String, u64>,
}

impl Seen {
    /// A record of nothing.
    pub const fn new() -> Self {
        Self {
            files: BTreeMap::new(),
        }
    }

    /// Remember that the file at `path` holds `contents`.
    pub fn note(&mut self, path: &str, contents: &str) {
        if self.files.len() >= MAX_FILES {
            self.files.clear();
        }
        self.files.insert(key(path), hash(contents));
    }

    /// Whether the file at `path` was read or written here before.
    pub fn knows(&self, path: &str) -> bool {
        self.files.contains_key(&key(path))
    }

    /// Whether the file at `path` held something other than `now` when it was last read or
    /// written here. A file never seen has not changed.
    pub fn changed(&self, path: &str, now: &str) -> bool {
        self.files
            .get(&key(path))
            .is_some_and(|then| *then != hash(now))
    }
}

/// `path` as it is remembered: `./a` and `a` are one file.
fn key(path: &str) -> String {
    let mut path = path;
    while let Some(rest) = path.strip_prefix("./") {
        path = rest.trim_start_matches('/');
    }
    path.to_owned()
}

/// 64-bit FNV-1a. It tells two texts apart and protects nothing.
fn hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_has_changed_only_if_it_was_seen_and_holds_something_else() {
        let mut seen = Seen::new();

        assert!(!seen.knows("a.txt"));
        assert!(!seen.changed("a.txt", "anything"));
        seen.note("a.txt", "one");
        assert!(seen.knows("./a.txt"));
        assert!(!seen.changed("a.txt", "one"));
        assert!(seen.changed("./a.txt", "two"));
        seen.note("a.txt", "two");
        assert!(!seen.changed("a.txt", "two"));
    }

    #[test]
    fn it_forgets_rather_than_grows_without_end() {
        let mut seen = Seen::new();
        for nth in 0..=MAX_FILES {
            seen.note(&format!("{nth}.txt"), "x");
        }

        assert!(!seen.knows("0.txt"));
        assert!(seen.knows(&format!("{MAX_FILES}.txt")));
    }
}
