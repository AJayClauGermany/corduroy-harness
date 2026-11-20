#!/bin/bash
# PostToolUse hook for Edit|Write|MultiEdit. After a change to Rust sources or the lint
# configuration, run `cargo lint` (strict clippy, warnings are errors) and feed any
# failure back to the agent.
#
# Exit 0: nothing to report (not a relevant file, or lint is clean).
# Exit 2: stderr is shown to Claude as feedback on the edit it just made.
#
# `set -e` is deliberately off: a failing lint must reach the reporting code below.
set -uo pipefail

# Hook input arrives as JSON on stdin. Edit/Write/MultiEdit all carry tool_input.file_path.
file=$(jq -r '.tool_input.file_path // .tool_response.filePath // empty' 2>/dev/null)

case "$file" in
  *.rs | Cargo.toml | */Cargo.toml | clippy.toml | */clippy.toml) ;;
  *) exit 0 ;;
esac

cd "${CLAUDE_PROJECT_DIR:-.}" || exit 0
command -v cargo >/dev/null 2>&1 || exit 0

max_lines=120

if output=$(cargo -q lint 2>&1); then
  exit 0
fi

total=$(printf '%s\n' "$output" | wc -l)
{
  echo "\`cargo lint\` failed after editing ${file}. Fix these findings (do not silence lints; see CLAUDE.md):"
  printf '%s\n' "$output" | sed -n "1,${max_lines}p"
  if [ "$total" -gt "$max_lines" ]; then
    echo "... truncated, ${total} lines in total. Run \`cargo lint\` for the rest."
  fi
} >&2
exit 2
