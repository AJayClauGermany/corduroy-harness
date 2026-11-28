# model-bench

Drives the `corduroy` command from outside, as another harness would, through a list of
tasks against a real model, several times each, and writes down what happened. What it found
with the local model is in [`docs/local-model-findings.md`](../../docs/local-model-findings.md).

It is not part of `cargo t` or CI: it needs a model and takes hours. `cargo eval` remains
the four quick evals; this is the long list.

## With nobody watching

One command, on Linux or macOS, from a fresh checkout:

```bash
scripts/model-bench/run.sh --models all --climb 0.25
```

It builds the command and its add-ons, builds the model server and fetches each model on
first use, serves the models one after another, and ends with them side by side. Nothing
asks a question on the way. What it says goes to `<out>/bench.log` as well; the comparison
is written to `<out>/summary.md`.

- **Left alone:** `nohup scripts/model-bench/run.sh --models all &`. On macOS the script
  keeps the machine awake (`caffeinate`) for as long as it runs.
- **Cut short** (a closed lid, a reclaimed container): run the same command again. A trial
  already in a results file is not run twice; `--fresh` forgets them instead.
- **A server that died** is started again before the next trial; a model that cannot be
  fetched or served is said so and the next one has its turn.
- **Root:** on Linux, run it as root or through `sudo` for the agent to be walled off (see
  below); the build is still done as you. On macOS it needs no root.
- **Time:** every task three times took one model most of a day on four CPU cores. `--climb`,
  `--tier` and `--trials` are what shorten it; a Mac with a GPU is several times faster.

The macOS side (the build with Metal, `sandbox-exec`, `caffeinate`) was written on Linux
and has not been run on a Mac yet: the first run there is its test. The bench checks its own
wall before it starts and says so if the sandbox does not hold. It also checks what the wall
leaves working: the tasks have the agent, and their own checks, run `python3`, `cc`, `make`,
`git` and `curl`, write a temporary file and listen on loopback, and a bench whose wall
breaks one of these stops before the first trial and names it. (A sandbox that denies
`/private/var/select` or the developer tools' directory turns macOS's `python3`, `cc`,
`make` and `git` into stubs that only print `xcode-select` errors.)

### On a Mac, behind the sandbox runtime

```bash
scripts/model-bench/run.sh --srt --provider anthropic --model-id claude-haiku-5-5 \
  --out .bench-runs/haiku
scripts/model-bench/bench.py --srt-check        # the wall alone, no model
```

`--srt` puts the agent's shell, and the checks that run what it wrote, under
[`srt`](https://github.com/anthropic-experimental/sandbox-runtime) (0.0.81 or later, with
`node`, both on `PATH`) through the shell kernel's `prefix`; the command itself, which talks
to the model, stays outside. `--out` has to be under `.bench-runs/` in this checkout. Behind
that wall nothing is read but the system's tools, macOS's developer tools (what
`/usr/bin/python3`, `cc`, `make` and `git` hand over to), the trial's workspace and a
scratch directory beside it, which holds the shell's `HOME` and `TMPDIR`; only those two are
written, and your own temporary directory (`/var/folders/.../T`), which `cc` and the
developer tools write to whatever `TMPDIR` says: what other programs of yours keep there is
in the agent's reach. Loopback is open, since a task starts a server and fetches from it, so a service of
your own on loopback is in reach too; nothing further out is. `--keychain-secret NAME` has
the command read the key from the keychain (`corduroy secrets set NAME`) and not from the
environment.

### Several at once

```bash
scripts/model-bench/run.sh --models all --jobs 4 --climb 0.25
```

`--jobs N` runs that many models side by side, each served on a port of its own (8080 and
up). The models here take about 9 GB between them at a 16k context, so memory is not what
limits it; the processor is. Models that share one are each slower, and what a task allows
in seconds (`--timeout`, 900 for a command) does not stretch: a model that passes alone may
time out beside three others. Compare runs made with the same `--jobs`, and raise
`--timeout` if the log shows commands being killed.

### What to hand over

Every run ends by writing two files into `<out>`, and `--compare <out>` writes them again
at any time (while runs are still going, too):

- `summary.md`: the comparison table.
- `collected.jsonl`: every record of every model under `<out>`, one a line, with the model
  and the platform in each. This is the one file to give to whoever analyses the runs.
  On Linux under `sudo` it is root's; copy it out.

The trials themselves (workspaces, each run's full event log) stay under `<out>/<model>/`
and are only needed to look closely at one failure.

To lay two machines side by side, give each its own `--out` (or copy `<out>` over) and
print the comparison for each: every record says which platform it ran on.

```bash
python3 scripts/model-bench/bench.py --compare /var/tmp/corduroy-bench
```

## One model, by hand

```bash
scripts/local-model.sh start
```

```bash
cargo build --release -p corduroy-cli && cargo plugins
```

```bash
sudo python3 scripts/model-bench/bench.py --trials 3
```

| Option | Meaning |
| --- | --- |
| `--models a,b` or `all` | serve each of these in turn (names: `scripts/local-model.sh models`); results go under `<out>/<model>` |
| `--jobs N` | run that many of `--models` at once, each on its own port |
| `--provider KIND --model-id ID` | a hosted model in the local one's place (see below); `--api-key-env` names its key's variable |
| `--climb RATE` | go tier by tier, and stop going up the ladder after a tier where less than this share of trials passed |
| `--list` | the tasks, with their tier |
| `--only a,b`, `--tier 1,2` | some of them |
| `--trials N` | how often each is tried (3). One pass or one failure says little |
| `--fresh` | forget earlier results instead of going on from them |
| `--compare DIR` | print the models under a directory side by side, and write `summary.md` and `collected.jsonl` there |
| `--summary FILE` | print the table for one results file again |
| `--label NAME` | what a model served by hand is called in its records |
| `--workspace-name NAME` | what the workspace directory is called (`workspace`) |
| `--out DIR` | where trials and results go (`/var/tmp/corduroy-bench`) |
| `--url`, `--binary`, `--plugins`, `--timeout`, `--user` | the endpoint, the command, its add-ons, seconds per command, who the agent runs as |

## The models

`scripts/local-model.sh models` lists the ones known by name, each a GGUF pinned by revision
and SHA-256 and served with the sampling its makers recommend:

| Name | Parameters | File | Notes |
| --- | --- | --- | --- |
| `lfm3-1.7b` | 1.7B | 4-bit, 1.1 GB | the one the [findings](../../docs/local-model-findings.md) are of |
| `qwen3-0.6b` | 0.6B | 8-bit, 0.6 GB | thinks before it answers |
| `qwen3-1.7b` | 1.7B | 8-bit, 1.8 GB | thinks before it answers |
| `qwen3.5-0.8b` | 0.8B | 4-bit, 0.5 GB | the generation after; answers without thinking by default. Quantized by Unsloth: Qwen publishes no GGUF of it |
| `qwen3.5-2b` | 1.9B | 4-bit, 1.3 GB | as above |
| `minicpm5-1b` | 1.1B | 4-bit, 0.7 GB | |
| `minicpm5-2b` | 2.5B | 4-bit, 1.6 GB | the largest here |

All are served with a 16k context, as the agent file states, so that the models differ and
the room they are given does not. To add one, add a line to `catalogue` in the script. To
run a model it does not know, serve it as an OpenAI-compatible endpoint and pass `--url`,
`--label` and an `--out` beside the others (`<out>/<label>`), so that `--compare` finds it.

Staying this small has two reasons. The sandbox the bench runs in has practical limits, and
a model of this size stays within them, slow as it is. And a small model shows whether the
harness makes up for what it lacks: a larger one does more and recovers from more, and would
hide what the harness leaves a model to work out for itself
([Design](../../docs/design.md#what-it-is-for)).

## The ladder

Tiers 1, 4, 5 and 6 are the ladder: each asks more of the model than the one before, and
how far up a model gets is what models are compared by (the comparison's last row but one).
Tiers 0, 2 and 3 ask about the harness and should hold whatever the model is.

The ladder is meant to end where a small hosted model (Claude Haiku) starts to fail: past
that, the question is no longer about models that run on one's own machine. Where that is
has not been measured, since no hosted model has been through the list; tier 6 is the top
for now, and a tier above it is to be written once one has.

## A hosted model beside them

```bash
python3 scripts/model-bench/bench.py --provider anthropic --model-id claude-haiku-5-5
```

The same agent with its provider's kind and its model's name changed, and nothing else:
the same prompt, tools, add-ons and limits, so that what differs is the model. Results go
under `<out>/claude-haiku-5-5` (or `--label`), where `--compare` finds them beside the
local ones. It can run while `--models` does: it needs no port and next to no processor.

- **The key** is read from `ANTHROPIC_API_KEY` in the bench's environment (`--api-key-env`
  names another variable) and handed to the command alone. The agent's shell starts from
  an empty environment and never has it; `environment_kept` checks that on every run.
  Under `sudo`, the variable has to survive it: `sudo --preserve-env=ANTHROPIC_API_KEY`.
- **Thinking** is off, as the agent file leaves it. That is the baseline to compare with;
  a second pass with thinking on would be a different column, not a better one of this.
- **Context** is the model's own, not the 16k the local models are held to: the tasks that
  ask what happens when a context overflows (`context_overflow`, `many_reads`, the
  compaction ones) ask a hosted model something easier. Read those rows with that in mind.
- **Cost**: each record has its tokens in and out. One pass of the whole list with the local
  model took a few million input tokens; try `--tier 1 --trials 1` first to see the rate.
- **What it does not test**: `request_timeout` and `output_limit` were written for a slow
  model; against a fast one a fifteen-second limit may never be reached.

## Models that only call tools

`needle_server.py` is an experiment with a different sort of model: Needle 3 (121M
parameters) is given tools and a request and answers with calls, never prose. The server
puts it behind the same endpoint and supplies the two things such a model lacks, with no
change to the harness: the run ends when the model calls nothing or calls `finish` (a tool
the server adds), and `finish` carries the answer. How to run it is at the top of the file.
It is not one of `--models`: its runtime is fetched by its own package, not pinned here.

## What keeps it honest

- **The agent cannot read its exam.** A shell kernel is not a sandbox, so under root on Linux
  the bench runs the agent as an unprivileged user (`corduroy-bench`, made on first use),
  closes this directory to that user, and refuses to start if the user can still list it.
  Finished trials are closed to it too, so a later trial cannot find an earlier answer. On
  macOS the agent runs under `sandbox-exec` with the same places denied, and finished trials
  are moved under `<out>/private/trials`. Where there is neither (Linux without root) there
  is no wall, and every record says `"isolated": false`.
- **Agents cannot read each other.** With `--jobs`, each model's agent is a user of its own
  (`corduroy-bench-1`, `-2`, ...) and a trial's directory is its agent's alone, so one model
  cannot find another's workspace while it is being worked in. A trial left open by a bench
  that was stopped is closed when the bench next starts.
- **Values are made per trial** (a code in a file, a port, a CSV), so nothing can be known
  beforehand, and a pass cannot be a memory of the last run.
- **Work is judged by tests the agent never saw**, run on a copy of its workspace, as its own
  user (never as root: it is code a model wrote).
- **Outcomes, not wording**: files, exit codes, the event log. The one exception is
  `honest_missing`, where the outcome is what the model says.
- **Several trials.** A task counts as passing or failing only when it does so every time;
  anything between is reported as its rate.

## Files

| File | What |
| --- | --- |
| `bench.py` | the runner: a `Trial` sets up a workspace and an agent file (the local-model example plus `search` and `today`), runs the command, reads the log |
| `tasks.py` | the tasks, in tiers: 0 the command itself, 1 one ability each, 2 gates and grants, 3 being driven (killed, stopped, continued), 4 real work, 5 past that, 6 awkward tools and larger programs |
| `mcp_vault.py` | a one-tool MCP server whose answer only it knows |
| `run.sh` | everything from a fresh checkout, with nobody watching |
| `needle_server.py` | a model that only calls tools, behind the same endpoint (an experiment) |

Each record in the results file has, besides pass or fail and why: the model and the platform, seconds, model requests,
tokens in and out and the largest request, every tool with how often it was called, every
tool error, every gate verdict that was not `allow`, every `capability/denied`, plugin
trouble, how each run ended, and what each command printed.

## Adding a task

A function in `tasks.py` under `@task(tier, "what it shows")`, taking a `Trial`:

```python
@task(1, "`read`: a value only the file holds")
def read(t):
    code = t.token()
    t.write("notes.txt", f"The deploy code is {code}.\n")
    finished(t, t.run("What is the deploy code in notes.txt?"))
    t.expect(code in t.answer(), "the answer lacks the code")
```

`t.expect` records a failure and goes on; `t.note` records something worth knowing that
decides nothing. Never write into the workspace anything that gives the check away.

## In pieces

Where a machine does not last as long as the bench (a cloud sandbox is reclaimed after about
two hours), `piece.sh MINUTES [options]` runs it for that long and keeps the results in
[`results/`](results/README.md), from where the next piece goes on. What has been kept so
far, and what it shows, is on that page.

## On GitHub's runners

[`.github/workflows/model-bench.yml`](../../.github/workflows/model-bench.yml) runs a piece
on a hosted runner: Actions, *Model bench*, *Run workflow*, with the models, tiers, trials
and minutes to spend. Each model is a job of its own, alone on its runner, so its times are
not those of two models sharing a sandbox's cores.

It is written to stay a small guest of a free service, and the limits are its own, set
under GitHub's (which change: see GitHub's documentation on Actions limits before raising
any of them):

| What | Here | Why |
| --- | --- | --- |
| When it runs | only when a person starts it | hours of a runner are never spent by a push |
| Runs at once | one; within it, two models at a time | a run of all seven does not take seven runners |
| A job's clock | `minutes` (at most 300) of bench, 345 in all | under the six hours a job is allowed, with what was found kept when the clock ends it |
| Downloads | the server's build and each model are cached | Hugging Face and PyPI are asked once a model; the caches (about 8 GB for all seven) fit the repository's allowance, and the least used are dropped first |
| What is kept | one artifact a model, 30 days | results are a few hundred kilobytes |
| Default | one model, tier 1, one trial | the smallest run that says something |

The workflow commits nothing. To keep a job's results, take `<model>.jsonl` from its
artifact into `results/` and commit it; the next piece, on a runner or anywhere else, goes
on from there. It has not been run yet: a workflow can only be started by hand once it is on
the default branch, so its first run is its test.
