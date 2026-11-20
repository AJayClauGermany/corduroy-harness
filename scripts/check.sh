#!/usr/bin/env bash
# Run every check CI runs, in the order that fails fastest.
# Usage: scripts/check.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

step() { printf '\n==> %s\n' "$*"; }

step "rustfmt"
cargo fmt --all -- --check

step "clippy (warnings are errors)"
cargo clippy --all-targets --all-features -- -D warnings

step "tests (nextest)"
cargo nextest run --all-features

# nextest does not run doctests.
step "doctests"
cargo test --doc --all-features

step "docs (warnings are errors)"
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features

printf '\nAll checks passed.\n'
