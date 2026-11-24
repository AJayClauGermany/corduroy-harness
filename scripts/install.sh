#!/bin/sh
# Install a ready-made build of corduroy (the command and the add-ons it ships with), with no
# Rust needed:
#
#   curl -fsSL https://raw.githubusercontent.com/AJayClauGermany/corduroy-harness/main/scripts/install.sh | sh
#
# Installs the latest build, checked against the release's SHA256SUMS, into
# ~/.local/share/corduroy and links ~/.local/bin/corduroy to it. Running it again replaces
# the installed build. Set, to change:
#
#   CORDUROY_BUILD   a release tag, such as build-1a2b3c4 (default: the latest)
#   CORDUROY_HOME    where the build is kept (default: ${XDG_DATA_HOME:-~/.local/share}/corduroy)
#   CORDUROY_BIN     where the command is linked (default: ~/.local/bin)
#   CORDUROY_REPO    the GitHub repository (default: AJayClauGermany/corduroy-harness)
set -eu

repo="${CORDUROY_REPO:-AJayClauGermany/corduroy-harness}"
build="${CORDUROY_BUILD:-latest}"
home_dir="${CORDUROY_HOME:-${XDG_DATA_HOME:-$HOME/.local/share}/corduroy}"
bin_dir="${CORDUROY_BIN:-$HOME/.local/bin}"

fail() {
  echo "corduroy install: $*" >&2
  exit 1
}

case "$(uname -s)/$(uname -m)" in
  Linux/x86_64 | Linux/amd64) target=x86_64-unknown-linux-gnu ;;
  Linux/aarch64 | Linux/arm64) target=aarch64-unknown-linux-gnu ;;
  Darwin/arm64) target=aarch64-apple-darwin ;;
  *) fail "no ready-made build for $(uname -s) $(uname -m); build from source (docs/tutorials/first-agent.md)" ;;
esac

if [ "$build" = latest ]; then
  base="https://github.com/$repo/releases/latest/download"
else
  base="https://github.com/$repo/releases/download/$build"
fi

if command -v curl > /dev/null; then
  fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget > /dev/null; then
  fetch() { wget -q "$1" -O "$2"; }
else
  fail "needs curl or wget"
fi
if command -v sha256sum > /dev/null; then
  sum() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum > /dev/null; then
  sum() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
  fail "needs sha256sum or shasum"
fi

archive="corduroy-$target.tar.gz"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

echo "Downloading $archive ($build) from $repo"
fetch "$base/$archive" "$scratch/$archive" || fail "could not download $base/$archive"
fetch "$base/SHA256SUMS" "$scratch/SHA256SUMS" || fail "could not download $base/SHA256SUMS"
expected="$(grep " $archive\$" "$scratch/SHA256SUMS" | cut -d' ' -f1)"
[ -n "$expected" ] || fail "$archive is not in SHA256SUMS"
[ "$(sum "$scratch/$archive")" = "$expected" ] || fail "$archive does not match its checksum"

tar -xzf "$scratch/$archive" -C "$scratch"
# Replace only an earlier build, never a directory that holds anything else.
if [ -e "$home_dir" ]; then
  [ -f "$home_dir/corduroy" ] && [ -d "$home_dir/plugins" ] ||
    fail "$home_dir exists and is not a corduroy build; remove it or set CORDUROY_HOME"
  rm -rf "$home_dir"
fi
mkdir -p "$(dirname "$home_dir")" "$bin_dir"
mv "$scratch/corduroy-$target" "$home_dir"
# A link, not a copy: the command finds its add-ons beside the file the link points to.
ln -sf "$home_dir/corduroy" "$bin_dir/corduroy"

echo "Installed $("$bin_dir/corduroy" --version) to $home_dir"
case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) echo "Add $bin_dir to your PATH to run it as \`corduroy\`." ;;
esac
echo "Next: set ANTHROPIC_API_KEY or OPENAI_API_KEY, then \`corduroy init\` in a folder to work in."
