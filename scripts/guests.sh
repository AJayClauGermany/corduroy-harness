#!/usr/bin/env bash
# Build the JavaScript and Python plugins in examples/, and the SDKs' own test plugins, with
# the SDKs in sdk/, into plugins/target/guests/. Needs Node.js 20+ and Python 3.11+; installs
# each SDK's toolchain (componentize-js, componentize-py) on first use. The host tests load
# the results.
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

step "Python SDK"
if [ ! -x sdk/python/.venv/bin/corduroy-py ]; then
  python3 -m venv sdk/python/.venv
  sdk/python/.venv/bin/pip install --quiet -e sdk/python
fi
sdk/python/.venv/bin/corduroy-py build examples/notes-py/notes.py \
  --out "$out/notes_py.wasm" --capabilities state,log,clock,random
# The SDK's own test plugin: tools whose results are content blocks.
sdk/python/.venv/bin/corduroy-py build sdk/python/tests/blocks.py \
  --out "$out/blocks_py.wasm" --capabilities tools

step "Script host"
node sdk/js/bin/corduroy-js.js build sdk/js/script-host/host.js --out "$out/script_host_js.wasm" \
  --capabilities fs-read,fs-write,kernel,events,log,clock,random,tools,state,service,plugins,llm,secret,http,log-read
