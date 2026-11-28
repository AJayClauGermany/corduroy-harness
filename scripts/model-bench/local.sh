#!/usr/bin/env bash
# The bench for local models, with nothing to steer it but what is listed here: for handing
# to something that is allowed to start a run and nothing else (an agent whose sandbox lets
# this one command out of it). Every argument is checked before anything is run, results go
# under .bench-runs/ in this checkout and nowhere else, and what the environment could
# change (another model server, another binary, a `PATH` into the tree) is taken away.
#
# Usage: scripts/model-bench/local.sh run MODELS NAME [trials=N] [tier=T,..] [only=TASK,..]
#                                         [climb=RATE] [fresh]
#        scripts/model-bench/local.sh models      the models that can be named
#        scripts/model-bench/local.sh tasks       the tasks that can be named
#        scripts/model-bench/local.sh check       the sandbox alone, no model (macOS)
#        scripts/model-bench/local.sh stop        stop the model server
#        scripts/model-bench/local.sh watch       do what is asked in .bench-runs/requests
#
# `watch` is for an agent that cannot be let out of its sandbox at all, only write under
# .bench-runs/. You start it in a terminal of your own and leave it; the agent writes
# .bench-runs/requests/ID.request holding one line, `run MODELS NAME [settings]` or `stop`,
# and reads ID.status (`running`, then `exit N`) and ID.log beside it. A request is checked
# exactly as the same words on this command line are, and they are done one at a time.
#
# MODELS is `all` or names from `models`, separated by commas. NAME names the run: its
# results are in .bench-runs/NAME. On macOS the agent's shell is behind the sandbox runtime
# (`--srt`); elsewhere run.sh's own wall applies (see README.md).
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

refuse() {
  printf 'local.sh: %s\n' "$1" >&2
  exit 2
}

# Nothing of the caller's choosing reaches the scripts below but the arguments checked here.
for name in $(env | sed -n 's/^\(CORDUROY_[A-Za-z0-9_]*\)=.*/\1/p'); do unset "$name"; done
unset CLAUDE_CODE_TMPDIR CLAUDE_TMPDIR DEVELOPER_DIR PYTHONPATH PYTHONSTARTUP
# A `PATH` entry that is relative, or inside this checkout, could put a program of the
# caller's in a tool's place.
kept=
IFS=: read -ra entries <<<"${PATH:-/usr/bin:/bin}"
for entry in "${entries[@]}"; do
  case $entry in
    "$root" | "$root"/*) ;;
    /*) kept=${kept:+$kept:}$entry ;;
  esac
done
export PATH=${kept:-/usr/bin:/bin}

# Whoever may write under .bench-runs/ could make a link there that leads out of it, and
# what is written "to the run" would land wherever it points.
plain() {
  [ ! -L "$1" ] || refuse "$1 is a link; it has to be a plain file or directory"
}

# One request from the queue: its line taken as words, never as shell, and handed to this
# script again, which checks them. The log and the status are made new each time, so that
# a link left under their names is not written through.
serve() {
  local id=$1 request=$queue/$1.request log=$queue/$1.log status=$queue/$1.status
  local line="" words=() code=0
  if [ -L "$request" ] || [ ! -f "$request" ]; then
    rm -f "$request"
    return
  fi
  IFS= read -r line <"$request" || true
  rm -f "$request" "$log" "$status"
  read -ra words <<<"${line:0:500}"
  printf '%s  %s: %s\n' "$(date '+%H:%M')" "$id" "${words[*]-}"
  (set -C && echo running >"$status") || return 0
  case ${words[0]-} in
    run | stop)
      (set -C && exec "$root/scripts/model-bench/local.sh" "${words[@]}" >"$log" 2>&1) || code=$?
      ;;
    *)
      (set -C && echo "local.sh: a request is \`run MODELS NAME [settings]\` or \`stop\`" >"$log") || true
      code=2
      ;;
  esac
  rm -f "$status"
  (set -C && echo "exit $code" >"$status") || true
  printf '%s  %s: exit %s\n' "$(date '+%H:%M')" "$id" "$code"
}

[ $# -ge 1 ] || refuse "say what to do: run, models, tasks, check, stop or watch"
action=$1
shift

case $action in
  models)
    [ $# -eq 0 ] || refuse "models takes no arguments"
    exec scripts/local-model.sh models
    ;;
  stop)
    [ $# -eq 0 ] || refuse "stop takes no arguments"
    # A run stops its own server when it ends, so there is often none left: for whoever
    # tidies up afterwards that is the state asked for, not a failure.
    said=$(scripts/local-model.sh stop 2>&1) && code=0 || code=$?
    case $said in
      *"no server started"*) echo "no model server is running" ;;
      *)
        [ -z "$said" ] || printf '%s\n' "$said"
        exit "$code"
        ;;
    esac
    exit 0
    ;;
  tasks)
    [ $# -eq 0 ] || refuse "tasks takes no arguments"
    exec python3 scripts/model-bench/bench.py --list
    ;;
  check)
    [ $# -eq 0 ] || refuse "check takes no arguments"
    [ "$(uname -s)" = Darwin ] || refuse "check is for macOS, where the sandbox runtime is used"
    exec python3 scripts/model-bench/bench.py --srt-check
    ;;
  watch)
    [ $# -eq 0 ] || refuse "watch takes no arguments"
    queue=.bench-runs/requests
    plain .bench-runs
    mkdir -p "$queue"
    plain "$queue"
    printf 'doing what is asked in %s, one request at a time; Ctrl-C stops\n' "$root/$queue"
    while :; do
      for request in "$queue"/*.request; do
        id=$(basename "$request" .request)
        if [[ $id =~ ^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$ ]]; then
          serve "$id"
        elif [ "$id" != "*" ]; then
          rm -f "$request"
        fi
      done
      sleep 2
    done
    ;;
  run) ;;
  *) refuse "unknown action \`$action\`: run, models, tasks, check, stop or watch" ;;
esac

[ $# -ge 2 ] || refuse "run needs the models and a name for the run"
models=$1 run=$2
shift 2

known=$(scripts/local-model.sh models | awk '{print $1}')
if [ "$models" != all ]; then
  [[ $models =~ ^[a-z0-9][a-z0-9.-]*(,[a-z0-9][a-z0-9.-]*)*$ ]] || refuse "models are names separated by commas, or \`all\`"
  IFS=, read -ra asked <<<"$models"
  for model in "${asked[@]}"; do
    grep -qxF -- "$model" <<<"$known" || refuse "no model \`$model\`: see \`local.sh models\`"
  done
fi
[[ $run =~ ^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$ ]] || refuse "a run's name is letters, digits, - and _ (64 at most)"

[ "$run" != requests ] || refuse "\`requests\` is where requests are left; name the run something else"
plain .bench-runs
plain ".bench-runs/$run"
if [ -d ".bench-runs/$run" ] && [ -n "$(find ".bench-runs/$run" -name trials -prune -o -type l -print -quit)" ]; then
  refuse ".bench-runs/$run holds a link; remove it or name the run something else"
fi

arguments=(--models "$models" --out ".bench-runs/$run")
for setting in "$@"; do
  case $setting in
    trials=*)
      [[ ${setting#trials=} =~ ^([1-9]|10)$ ]] || refuse "trials is 1 to 10"
      arguments+=(--trials "${setting#trials=}")
      ;;
    tier=*)
      [[ ${setting#tier=} =~ ^[0-6](,[0-6])*$ ]] || refuse "tier is tiers 0 to 6, separated by commas"
      arguments+=(--tier "${setting#tier=}")
      ;;
    only=*)
      [[ ${setting#only=} =~ ^[a-z][a-z0-9_]*(,[a-z][a-z0-9_]*)*$ ]] || refuse "only is task names, separated by commas"
      arguments+=(--only "${setting#only=}")
      ;;
    climb=*)
      [[ ${setting#climb=} =~ ^0\.[0-9]{1,2}$ ]] || refuse "climb is a share such as 0.25"
      arguments+=(--climb "${setting#climb=}")
      ;;
    fresh) arguments+=(--fresh) ;;
    *) refuse "unknown setting \`$setting\`: trials=, tier=, only=, climb= or fresh" ;;
  esac
done
needed=(cargo python3 git)
if [ "$(uname -s)" = Darwin ]; then
  arguments+=(--srt)
  needed+=(srt node)
fi
# Said here, with the `PATH` that was searched: whoever starts this may have a shorter one
# than the person's own shell.
for tool in "${needed[@]}"; do
  command -v "$tool" >/dev/null || refuse "\`$tool\` is not on PATH, which is $PATH"
done

exec scripts/model-bench/run.sh "${arguments[@]}"
