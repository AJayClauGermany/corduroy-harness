//! Turning the paths a plugin names into the paths the host checks and opens.

use std::path::{Component, Path, PathBuf};

/// Resolve `path` to an absolute path with no `.`, `..` or symbolic links in it, so a scope
/// check on the result cannot be sidestepped.
///
/// Relative paths are taken from `root`. Symbolic links are followed for the part of the path
/// that exists; the remainder (a file about to be created, say) is appended as written.
pub(super) fn resolve(root: &Path, path: &str) -> PathBuf {
    let joined = root.join(path);
    let absolute = std::path::absolute(&joined).unwrap_or(joined);

    // Fold `.` and `..` lexically first, so `..` cannot be hidden behind a missing directory.
    let mut folded = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                folded.pop();
            }
            other => folded.push(other),
        }
    }

    // Then follow links in the longest prefix that exists.
    let mut existing = folded.as_path();
    let mut missing = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            return missing
                .iter()
                .rev()
                .fold(real, |path, name| path.join(name));
        }
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
            return folded;
        };
        missing.push(name);
        existing = parent;
    }
}

/// The form of `path` that scopes are matched against: `/`-separated text.
pub(super) fn scope_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("corduroy-paths-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn relative_paths_resolve_under_the_root_and_dots_are_folded() {
        let root = scratch("dots");
        fs::create_dir_all(root.join("a")).unwrap();

        assert_eq!(resolve(&root, "a/./b/../c.txt"), root.join("a/c.txt"));
        assert_eq!(
            resolve(&root, "a/../../escape"),
            root.parent().unwrap().join("escape")
        );
        assert_eq!(
            resolve(&root, "/etc/hostname"),
            PathBuf::from("/etc/hostname")
        );
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_are_followed_before_the_scope_check() {
        let root = scratch("links");
        let outside = scratch("links-outside");
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        assert_eq!(resolve(&root, "link/new.txt"), outside.join("new.txt"));
        assert_eq!(scope_key(&outside), outside.to_string_lossy());
    }
}
