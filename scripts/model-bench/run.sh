#!/usr/bin/env bash
# The bench from nothing, with nobody watching: builds the command and its add-ons, then
# hands over to bench.py, which builds the model server, fetches each model, serves them one
# after another and writes the comparison at the end. Everything is said to the log as well
# as the terminal, and a run that was cut short goes on from where it stopped when this is
# run again with the same arguments.
#
# Usage: scripts/model-bench/run.sh [bench.py's options]
#        scripts/model-bench/run.sh --models all --climb 0.25
#        nohup scripts/model-bench/run.sh --models qwen3-0.6b,lfm3-1.7b --tier 1,4 &
#        scripts/model-bench/run.sh --models all --jobs 4      several models side by side
#        scripts/model-bench/run.sh --provider anthropic --model-id claude-haiku-5-5
#
# Without --models it runs every model `scripts/local-model.sh models` lists. On Linux, run
# it as root (or through sudo) for the agent to be walled off from the checks; on macOS it
# needs no root. The log is <out>/bench.log (out: /var/tmp/corduroy-bench, or --out).
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

out=/var/tmp/corduroy-bench
models=
previous=
for argument in "$@"; do
  [ "$previous" = --out ] && out=$argument
  case $argument in --models | --models=* | --provider | --provider=*) models=yes ;; --out=*) out=${argument#--out=} ;; esac
  previous=$argument
done
mkdir -p "$out"
log=$out/bench.log

# Under sudo, build as the person who owns the tree, so that nothing in target/ becomes root's.
build() {
  if [ "$(id -u)" = 0 ] && [ -n "${SUDO_USER:-}" ]; then sudo -u "$SUDO_USER" -H "$@"; else "$@"; fi
}

{
  printf '\n==> %s: %s on %s %s\n' "$(date '+%Y-%m-%d %H:%M')" \
    "$(git rev-parse --short HEAD 2>/dev/null || echo '?')" "$(uname -s)" "$(uname -m)"
  # Where the command and its add-ons were built beforehand (the workflow does, as the
  # runner's user, before it becomes root), CORDUROY_BENCH_BUILT says so.
  if [ -z "${CORDUROY_BENCH_BUILT:-}" ]; then
    build cargo build --release -p corduroy-cli
    build cargo plugins
  fi
  # A laptop left alone goes to sleep; `caffeinate` (macOS) keeps it awake while this runs.
  awake=()
  command -v caffeinate >/dev/null && awake=(caffeinate -i)
  [ -n "$models" ] || set -- --models all "$@"
  ${awake[@]+"${awake[@]}"} python3 -u scripts/model-bench/bench.py "$@"
} 2>&1 | tee -a "$log"
