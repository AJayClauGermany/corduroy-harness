#!/usr/bin/env bash
# Package a built `corduroy` command with the add-ons it ships with, as one archive that runs
# where it is unpacked: the command finds its add-ons in the `plugins/` directory beside it.
#
#   scripts/package.sh <binary> <add-on dir> <name> [out dir]
#
# <binary>      the built command (target/<triple>/release/corduroy[.exe])
# <add-on dir>  the built add-ons (plugins/target/wasm32-unknown-unknown/release)
# <name>        the archive's name, also its top directory (corduroy-x86_64-unknown-linux-gnu)
# [out dir]     where the archive goes (dist)
#
# Writes <out dir>/<name>.zip if the binary ends in .exe, else <out dir>/<name>.tar.gz, and
# prints its path. `scripts/package.sh addons <add-on dir> <name> [out dir]` packages the
# add-ons alone. CORDUROY_COMMIT, if set, is written to the archive's COMMIT file.
set -euo pipefail

binary="${1:?usage: package.sh <binary|addons> <add-on dir> <name> [out dir]}"
addons="${2:?add-on dir missing}"
name="${3:?name missing}"
out="${4:-dist}"
root="$(cd "$(dirname "$0")/.." && pwd)"

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
dir="$stage/$name"
mkdir -p "$dir/plugins" "$out"
out="$(cd "$out" && pwd)"

# Every add-on built from plugins/ except `testbed`, which is for the host's tests only.
shopt -s nullglob
for addon in "$addons"/*.wasm; do
  [ "$(basename "$addon")" = testbed.wasm ] || cp "$addon" "$dir/plugins/"
done
if [ -z "$(ls "$dir/plugins")" ]; then
  echo "no add-ons in $addons: build them first (cargo plugins)" >&2
  exit 1
fi
cp "$root/LICENSE" "$dir/"
if [ -n "${CORDUROY_COMMIT:-}" ]; then
  printf '%s\n' "$CORDUROY_COMMIT" > "$dir/COMMIT"
fi

if [ "$binary" = addons ]; then
  # The add-ons alone, for a command built from source or installed some other way: unpack
  # anywhere and point CORDUROY_PLUGIN_PATH at the plugins directory.
  archive="$out/$name.tar.gz"
  tar -C "$stage" -czf "$archive" "$name"
  printf '%s\n' "$archive"
  exit 0
fi

exe="$(basename "$binary")"
cp "$binary" "$dir/$exe"
cat > "$dir/README.md" <<EOF
# corduroy

A build of [corduroy](https://github.com/AJayClauGermany/corduroy-harness)${CORDUROY_COMMIT:+ from commit \`$CORDUROY_COMMIT\`}.

\`$exe\` is the command; \`plugins/\` holds the add-ons it ships with (WebAssembly), found
there wherever this directory is. Keep them together.

1. Choose a model: set \`ANTHROPIC_API_KEY\` or \`OPENAI_API_KEY\`, or run Ollama locally.
2. In the folder the agent should work in, run \`$exe init\` to write \`corduroy.toml\`.
3. \`$exe check\` lists what the agent has; \`$exe\` talks to it.

Start with the tutorial: docs/tutorials/first-agent.md in the repository.
EOF

if [[ "$exe" == *.exe ]]; then
  archive="$out/$name.zip"
  rm -f "$archive"
  if command -v 7z > /dev/null; then
    (cd "$stage" && 7z a -tzip -bso0 "$archive" "$name")
  else
    (cd "$stage" && zip -qr "$archive" "$name")
  fi
else
  archive="$out/$name.tar.gz"
  tar -C "$stage" -czf "$archive" "$name"
fi
printf '%s\n' "$archive"
