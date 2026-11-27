#!/usr/bin/env bash
# The bench in pieces, for a machine that does not last: a cloud sandbox is reclaimed after
# about two hours, and everything under /var/tmp goes with it. This runs the bench for a
# stated number of minutes and keeps what it found in the repository, under
# scripts/model-bench/results/, so that the next piece, in a fresh container, goes on from
# where this one stopped. A trial already recorded is never run twice (see README.md).
#
# Usage: scripts/model-bench/piece.sh MINUTES [run.sh's options]
#        scripts/model-bench/piece.sh 80 --models all --tier 0,1 --trials 1
#        scripts/model-bench/piece.sh restore    only put the kept results back under <out>
#        scripts/model-bench/piece.sh save       only keep what is under <out>
#
# One piece: the kept results are put back under <out>, the bench runs, and every five
# minutes and at the end the results are kept again. A trial cut off by the clock is not
# recorded and is tried again by the next piece. Commit and push `results/` after each
# piece: that is what outlives the container.
#
# Kept per model: `results/<model>.jsonl` (the bench's own results file, which is what makes
# going on possible), and for all of them `results/summary.md` and `results/collected.jsonl`.
# Workspaces and full event logs are not kept; they stay under <out> until the container goes.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
kept=$here/results
out=/var/tmp/corduroy-bench
previous=
for argument in "$@"; do
  [ "$previous" = --out ] && out=$argument
  case $argument in --out=*) out=${argument#--out=} ;; esac
  previous=$argument
done

lines() { if [ -f "$1" ]; then wc -l <"$1"; else echo 0; fi; }

# The longer file wins on both sides: results are only ever added to.
restore() {
  mkdir -p "$kept"
  for file in "$kept"/*.jsonl; do
    [ -f "$file" ] || continue
    model=$(basename "$file" .jsonl)
    [ "$model" = collected ] && continue
    target=$out/$model/private/results.jsonl
    if [ "$(lines "$file")" -gt "$(lines "$target")" ]; then
      mkdir -p "$(dirname "$target")"
      cp "$file" "$target"
    fi
  done
}

save() {
  mkdir -p "$kept"
  for file in "$out"/*/private/results.jsonl; do
    [ -f "$file" ] || continue
    model=$(basename "$(dirname "$(dirname "$file")")")
    [ "$(lines "$file")" -gt "$(lines "$kept/$model.jsonl")" ] && cp "$file" "$kept/$model.jsonl"
  done
  python3 "$here/bench.py" --compare "$out" >/dev/null 2>&1 || true
  for file in summary.md collected.jsonl; do
    [ -f "$out/$file" ] && cp "$out/$file" "$kept/$file"
  done
  return 0
}

case ${1:-} in
  restore) restore; exit 0 ;;
  save) save; exit 0 ;;
  '' | *[!0-9]*) sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//' >&2; exit 2 ;;
esac
minutes=$1
shift

restore
( while sleep 300; do save; done ) &
saver=$!
trap 'kill "$saver" 2>/dev/null || true; save' EXIT
# The clock ends the bench, not an error: what was recorded is kept either way.
timeout --signal=TERM --kill-after=30 "${minutes}m" "$here/run.sh" "$@" || true
# A server the bench was serving when the clock ran out is still there.
pkill -f 'llama-server' 2>/dev/null || true
