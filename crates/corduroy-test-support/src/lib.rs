//! Helpers shared by the integration tests of the corduroy workspace.
//!
//! The guest plugins under `plugins/` are built separately, with `cargo plugins`. Tests find
//! the results through [`plugin`], which refuses to hand out an artifact that is missing or
//! older than its sources, so a test never silently runs against a stale plugin.
//!
//! This crate is test-only: its functions panic on failure rather than return errors.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The repository root: the nearest ancestor of this crate that holds the WIT world.
fn root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .find(|dir| dir.join("wit/corduroy.wit").is_file())
        .unwrap_or(manifest)
        .to_owned()
}

/// The most recent modification time of any file below `dir`.
fn newest(dir: &Path) -> SystemTime {
    let Ok(entries) = fs::read_dir(dir) else {
        return SystemTime::UNIX_EPOCH;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .map(|path| {
            if path.is_dir() {
                newest(&path)
            } else {
                modified(&path).unwrap_or(SystemTime::UNIX_EPOCH)
            }
        })
        .max()
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

/// Path to the built plugin `name`, a crate under `plugins/`.
///
/// # Panics
///
/// Panics with instructions if the plugin has not been built, or was built before its
/// sources or the WIT world last changed.
#[must_use]
pub fn plugin(name: &str) -> PathBuf {
    let root = root();
    let artifact = root.join(format!(
        "plugins/target/wasm32-unknown-unknown/release/{}.wasm",
        name.replace('-', "_")
    ));
    let built = modified(&artifact);
    assert!(
        built.is_some(),
        "plugin `{name}` is not built: run `cargo plugins` first"
    );
    let sources =
        newest(&root.join("plugins").join(name).join("src")).max(newest(&root.join("wit")));
    assert!(
        built >= Some(sources),
        "plugin `{name}` is older than its sources: run `cargo plugins` again"
    );
    artifact
}

/// A fresh, empty directory for the test called `test`, with symbolic links resolved so its
/// path can be used in capability scopes.
///
/// # Panics
///
/// Panics if the directory cannot be created.
#[must_use]
pub fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("corduroy-{test}-{}", std::process::id()));
    assert!(
        !dir.exists() || fs::remove_dir_all(&dir).is_ok(),
        "cannot clear {}",
        dir.display()
    );
    assert!(
        fs::create_dir_all(&dir).is_ok(),
        "cannot create {}",
        dir.display()
    );
    dir.canonicalize().unwrap_or(dir)
}
