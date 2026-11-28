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

[ $# -ge 1 ] || refuse "say what to do: run, models, tasks, check or stop"
action=$1
shift

case $action in
  models)
    [ $# -eq 0 ] || refuse "models takes no arguments"
    exec scripts/local-model.sh models
    ;;
  stop)
    [ $# -eq 0 ] || refuse "stop takes no arguments"
    exec scripts/local-model.sh stop
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
  run) ;;
  *) refuse "unknown action \`$action\`: run, models, tasks, check or stop" ;;
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
[ "$(uname -s)" = Darwin ] && arguments+=(--srt)

exec scripts/model-bench/run.sh "${arguments[@]}"
