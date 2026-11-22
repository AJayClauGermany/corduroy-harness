#!/usr/bin/env bash
# Build the JavaScript and Python plugins in examples/ with the SDKs in sdk/, into
# plugins/target/guests/. Needs Node.js 20+ and Python 3.11+; installs each SDK's toolchain
# (componentize-js, componentize-py) on first use. The host tests load the results.
# Usage: scripts/guests.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
out=plugins/target/guests
mkdir -p "$out"

step() { printf '\n==> %s\n' "$*"; }

step "JavaScript SDK"
if [ ! -d sdk/js/node_modules ]; then
  (cd sdk/js && npm ci --no-audit --no-fund)
fi
node sdk/js/bin/corduroy-js.js build examples/notes-js/plugin.js \
  --out "$out/notes_js.wasm" --capabilities state,log,clock,random
