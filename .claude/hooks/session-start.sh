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

# cargo-nextest, used by `cargo t`. The prebuilt binaries at get.nexte.st are blocked by
# the cloud egress policy, so build from crates.io (slow, ~3-4 min, but cached with the
# container afterwards and skipped on every later session start).
if ! cargo nextest --version >/dev/null 2>&1; then
  cargo install cargo-nextest --locked >&2
fi
