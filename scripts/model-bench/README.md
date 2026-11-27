# model-bench

Drives the `corduroy` command from outside, as another harness would, through a list of
tasks against a real model, several times each, and writes down what happened. What it found
with the local model is in [`docs/local-model-findings.md`](../../docs/local-model-findings.md).

It is not part of `cargo t` or CI: it needs a model and takes hours. `cargo eval` remains
the four quick evals; this is the long list.

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
| `--list` | the tasks, with their tier |
| `--only a,b`, `--tier 1,2` | some of them |
| `--trials N` | how often each is tried (3). One pass or one failure says little |
| `--workspace-name NAME` | what the workspace directory is called (`workspace`) |
| `--out DIR` | where trials and results go (`/var/tmp/corduroy-bench`) |
| `--summary FILE` | print the table for a results file again |
| `--url`, `--binary`, `--plugins`, `--timeout`, `--user` | the endpoint, the command, its add-ons, seconds per command, who the agent runs as |

## What keeps it honest

- **The agent cannot read its exam.** A shell kernel is not a sandbox, so under root the
  bench runs the agent as an unprivileged user (`corduroy-bench`, made on first use), closes
  this directory to that user, and refuses to start if the user can still list it. Finished
  trials are closed to it too, so a later trial cannot find an earlier answer. Without root
  there is no wall, and every record says `"isolated": false`.
- **Values are made per trial** (a code in a file, a port, a CSV), so nothing can be known
  beforehand, and a pass cannot be a memory of the last run.
- **Work is judged by tests the agent never saw**, run on a copy of its workspace, as its own
  user (never as root: it is code a model wrote).
- **Outcomes, not wording**: files, exit codes, the event log. The one exception is
  `honest_missing`, where the outcome is what the model says.
- **Several trials.** A task counts as passing or failing only when it does so every time;
  anything between is reported as its rate.

## Models to try next

One model has been through the list, so nothing yet separates what is that model's from
what any small model does. The next passes are for other very small models, under about
two billion parameters, and for ones that do not think before they answer. Candidates, as
they were named and to be checked before use (the exact names, sizes and whether a build
for `llama-server` exists): a MiniCPM variant at about one billion parameters and one at
about two, and a Qwen 3 variant at about 0.9 billion.

Two reasons for staying this small. The sandbox the bench runs in has practical limits,
and a model of this size stays within them, slow as it is. And a small model shows whether
the harness makes up for what it lacks: a larger one does more and recovers from more, and
would hide what the harness leaves a model to work out for itself
([Design](../../docs/design.md#what-it-is-for)).

To run another model, serve it as an OpenAI-compatible endpoint and pass `--url`; give the
results another `--out`, so that passes can be laid side by side.

## Files

| File | What |
| --- | --- |
| `bench.py` | the runner: a `Trial` sets up a workspace and an agent file (the local-model example plus `search` and `today`), runs the command, reads the log |
| `tasks.py` | the tasks, in tiers: 0 the command itself, 1 one ability each, 2 gates and grants, 3 being driven (killed, stopped, continued), 4 real work, 5 past that |
| `mcp_vault.py` | a one-tool MCP server whose answer only it knows |

Each record in the results file has, besides pass or fail and why: seconds, model requests,
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
