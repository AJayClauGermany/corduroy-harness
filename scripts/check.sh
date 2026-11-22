#!/usr/bin/env bash
# Run every check CI runs, in the order that fails fastest.
# Usage: scripts/check.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

step() { printf '\n==> %s\n' "$*"; }

# The guest plugins are a separate workspace that targets WebAssembly.
plugins=(--manifest-path plugins/Cargo.toml)
wasm=(--target wasm32-unknown-unknown)

step "rustfmt"
cargo fmt --all -- --check
cargo fmt "${plugins[@]}" --all -- --check

step "clippy (warnings are errors)"
cargo clippy --all-targets --all-features -- -D warnings
# The core and the loop without the standard library, on a microcontroller, and the harness
# with the standard library but without tokio, WebAssembly, the shell or a log file.
cargo clippy -p corduroy-core --no-default-features --features critical-section \
  --target thumbv7em-none-eabihf -- -D warnings
cargo clippy -p corduroy-harness --no-default-features --features critical-section \
  --target thumbv7em-none-eabihf -- -D warnings
cargo clippy -p corduroy-harness --all-targets --no-default-features --features std -- -D warnings
cargo clippy "${plugins[@]}" "${wasm[@]}" -- -D warnings

# The host tests load these, so they are built first.
step "plugins (wasm32)"
cargo plugins

# JavaScript and Python plugins, if Node.js and Python are here; their tests skip otherwise.
if command -v node >/dev/null && command -v python3 >/dev/null; then
  step "guest-language plugins"
  scripts/guests.sh
  export CORDUROY_REQUIRE_GUESTS=1
else
  step "guest-language plugins: skipped (needs node and python3; their tests will skip)"
fi

step "tests (nextest)"
cargo nextest run --all-features

step "plugin unit tests"
cargo test "${plugins[@]}"

# nextest does not run doctests.
step "doctests"
cargo test --doc --all-features

step "docs (warnings are errors)"
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features

printf '\nAll checks passed.\n'
