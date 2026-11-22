//! Helpers shared by the integration tests of the corduroy workspace.
//!
//! The guest plugins under `plugins/` are built separately, with `cargo plugins`. Tests find
//! the results through [`plugin`], which refuses to hand out an artifact that is missing or
//! older than its sources, so a test never silently runs against a stale plugin.
//!
//! Plugins written in JavaScript and Python are built with `scripts/guests.sh`, which needs
//! Node.js and Python; tests find them through [`guest`], and skip when they are not built
//! unless `CORDUROY_REQUIRE_GUESTS` is set, as it is in CI.
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

/// Directories of installed packages and caches, which are not sources.
const NOT_SOURCES: [&str; 4] = ["node_modules", ".venv", "__pycache__", "target"];

/// The most recent modification time of any file below `dir`.
fn newest(dir: &Path) -> SystemTime {
    let Ok(entries) = fs::read_dir(dir) else {
        return SystemTime::UNIX_EPOCH;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| !NOT_SOURCES.iter().any(|name| entry.file_name() == *name))
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
/// crate, the guest crate or the WIT world last changed.
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
    // The plugin's own crate, the guest crate every plugin builds on, and the world.
    let plugins = root.join("plugins");
    let sources = [plugins.join(name), plugins.join("guest"), root.join("wit")]
        .iter()
        .map(|path| newest(path))
        .max()
        .unwrap_or(SystemTime::UNIX_EPOCH);
    assert!(
        built >= Some(sources),
        "plugin `{name}` is older than its sources: run `cargo plugins` again"
    );
    artifact
}

/// Path to the built guest-language plugin `name` (`notes_js`, say), built by
/// `scripts/guests.sh` from `examples/` with the SDKs in `sdk/`. `None` if it has not been
/// built, so the test can skip.
///
/// # Panics
///
/// Panics with instructions if it is older than the SDKs, the examples or the WIT world, or
/// if it is missing while `CORDUROY_REQUIRE_GUESTS` is set.
#[must_use]
pub fn guest(name: &str) -> Option<PathBuf> {
    let root = root();
    let artifact = root.join(format!("plugins/target/guests/{name}.wasm"));
    let Some(built) = modified(&artifact) else {
        assert!(
            std::env::var_os("CORDUROY_REQUIRE_GUESTS").is_none(),
            "guest plugin `{name}` is not built: run `scripts/guests.sh` first"
        );
        return None;
    };
    let sources = ["sdk", "examples", "wit"]
        .iter()
        .map(|dir| newest(&root.join(dir)))
        .max()
        .unwrap_or(SystemTime::UNIX_EPOCH);
    assert!(
        built >= sources,
        "guest plugin `{name}` is older than its sources: run `scripts/guests.sh` again"
    );
    Some(artifact)
}

/// A directory for wasmtime's compiled-code cache shared by every test process, so a large
/// component is compiled once per change rather than once per test.
#[must_use]
pub fn compile_cache() -> PathBuf {
    root().join("target/wasm-cache-tests")
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
