#!/bin/bash
# SessionStart hook for Claude Code cloud sessions: make `cargo t` and `cargo lint`
# work immediately. Idempotent; each step installs only what is missing.
#
# Stdout from a SessionStart hook is added to the model's context, so everything
# noisy is sent to stderr.
set -euo pipefail

# Local machines manage their own toolchain; only act in cloud sessions.
if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

cd "${CLAUDE_PROJECT_DIR:-.}"

# Components that rust-toolchain.toml asks for: clippy (`cargo lint`) and rustfmt (`cargo fmtc`).
if command -v rustup >/dev/null 2>&1; then
  installed_components=$(rustup component list --installed)
  for component in clippy rustfmt; do
    if ! grep -q "^${component}-" <<<"$installed_components"; then
      rustup component add "$component" >&2
    fi
  done
fi

# The WebAssembly target the guest plugins in plugins/ are built for, and the microcontroller
# target scripts/check.sh lints the core for without the standard library.
if command -v rustup >/dev/null 2>&1; then
  installed_targets=$(rustup target list --installed)
  for target in wasm32-unknown-unknown thumbv7em-none-eabihf; do
    if ! grep -q "^${target}\$" <<<"$installed_targets"; then
      rustup target add "$target" >&2
    fi
  done
fi

# cargo-nextest, used by `cargo t`. The prebuilt binaries at get.nexte.st are blocked by
# the cloud egress policy, so build from crates.io (slow, ~3-4 min, but cached with the
# container afterwards and skipped on every later session start).
if ! cargo nextest --version >/dev/null 2>&1; then
  cargo install cargo-nextest --locked >&2
fi

# Build the guest plugins so the tests that load them pass straight away (~30s cold).
cargo plugins >&2

# The JavaScript and Python plugins, so their tests run instead of skipping (~40s the first
# time: installs componentize-js and componentize-py). A failure here does not block the
# session; those tests then skip.
if command -v node >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; then
  scripts/guests.sh >&2 || echo "scripts/guests.sh failed; the guest-language tests will skip" >&2
fi
