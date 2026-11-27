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
wall before it starts and says so if the sandbox does not hold. Two tasks (`interrupt`,
`background_server`) clean up with `pkill` by name, which on a machine that is also your
desk reaches any `sleep` of your own.

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
| `--climb RATE` | go tier by tier, and stop going up the ladder after a tier where less than this share of trials passed |
| `--list` | the tasks, with their tier |
| `--only a,b`, `--tier 1,2` | some of them |
| `--trials N` | how often each is tried (3). One pass or one failure says little |
| `--fresh` | forget earlier results instead of going on from them |
| `--compare DIR` | print the models under a directory side by side |
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
