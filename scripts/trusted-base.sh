#!/usr/bin/env bash
# Measure the trusted base: the code that enforces the rules whatever the plugins become.
#
# A prototype of the measure A4 in docs/open-questions.md asks for (see docs/prototypes.md).
# That the trusted base is kept to a budget is decided; its figures and the check that holds
# them are not, so this only measures. It is not part of scripts/check.sh.
#
# Usage: scripts/trusted-base.sh            print the figures
#        scripts/trusted-base.sh --budget   compare them with scripts/trusted-base.budget,
#                                           and fail if one is over
#
# The trusted base is taken as the design's core tier: corduroy-core, the plugin world
# (wit/corduroy.wit), the WebAssembly host (corduroy-wasm) and the loop (corduroy-harness).
# Lines are lines of code: not blank, not a comment, and not a file's unit tests. Crates are
# what those three need to build for this machine, their own and others' counted apart, with
# the WebAssembly engine among them.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

# Lines of code in the Rust sources under the directories given.
lines() {
  find "$@" -name '*.rs' -print0 | xargs -0 awk '
    FNR == 1 { tests = 0 }
    /^#\[cfg\(test\)\]/ { tests = 1 }
    tests { next }
    /^[[:space:]]*$/ || /^[[:space:]]*\/\// { next }
    { count++ }
    END { print count + 0 }'
}

# The crates `cargo tree` lists for the arguments given, the workspace's own left out.
crates() {
  cargo tree "$@" --edges normal --prefix none --format '{p}' 2>/dev/null \
    | awk 'NF && $1 !~ /^corduroy-/ { print $1 }' | sort -u | wc -l | tr -d ' '
}

core_lines=$(lines crates/corduroy-core/src)
wasm_lines=$(lines crates/corduroy-wasm/src)
loop_lines=$(lines src)
wit_lines=$(grep -cv -e '^[[:space:]]*$' -e '^[[:space:]]*//' wit/corduroy.wit)
total_lines=$((core_lines + wasm_lines + loop_lines + wit_lines))
core_crates=$(crates -p corduroy-core)
loop_crates=$(crates -p corduroy-harness --no-default-features --features std,tokio)
all_crates=$(crates -p corduroy-harness -p corduroy-wasm)

figures=$(cat <<FIGURES
lines.core $core_lines
lines.wasm-host $wasm_lines
lines.loop $loop_lines
lines.world $wit_lines
lines.total $total_lines
crates.core $core_crates
crates.without-add-ons $loop_crates
crates.with-add-ons $all_crates
FIGURES
)

if [[ "${1:-}" != "--budget" ]]; then
  printf '%s\n' "$figures"
  exit 0
fi

over=0
while read -r name figure; do
  budget=$(awk -v name="$name" '$1 == name { print $2 }' scripts/trusted-base.budget)
  if [[ -z "$budget" ]]; then
    printf '%-24s %6s   no budget\n' "$name" "$figure"
  elif (( figure > budget )); then
    printf '%-24s %6s   over its budget of %s\n' "$name" "$figure" "$budget"
    over=1
  else
    printf '%-24s %6s   within %s\n' "$name" "$figure" "$budget"
  fi
done <<<"$figures"
exit "$over"
