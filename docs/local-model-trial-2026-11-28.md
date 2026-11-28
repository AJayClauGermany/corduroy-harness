# A trial run of every local model

One pass of `cargo eval` (the four quick evals in
[`crates/corduroy-cli/tests/local_model.rs`](../crates/corduroy-cli/tests/local_model.rs))
against each model [`scripts/local-model.sh`](../scripts/local-model.sh) knows by name, on a
four-core, 15 GB Linux sandbox with no GPU, on 28 November 2026. Each model was served at the
script's defaults (16k context, its makers' sampling) and driven through
[`examples/local-model/corduroy.toml`](../examples/local-model/corduroy.toml), unchanged.

It is a smoke test, not a benchmark. **Every cell is one trial** at a sampling temperature
above zero, so a pass or a fail is a sample, not a rate; the 75-task, repeated runs behind
[what a small local model does](local-model-findings.md) (LFM3 only) are the better
instrument, and
[`scripts/model-bench/`](../scripts/model-bench/README.md) is how to get the same for the
other models. What this page adds is the first look at all seven side by side, a defect in the
evals themselves, and what each failure looked like, so the models can be reviewed one by one.

## Result

| Model | Size | `sum` | `fix` | `summarize` | `memory` | Passed |
| --- | --- | --- | --- | --- | --- | --- |
| `lfm3-1.7b` | 1.7B | pass 23 s | pass 58 s | pass 41 s | pass 29 s | **4/4** |
| `qwen3.5-2b` | 1.9B | pass 26 s | pass 49 s | pass 59 s | pass 26 s | **4/4** |
| `minicpm5-2b` | 2.5B | pass 32 s | pass 68 s | fail 47 s | pass 46 s | 3/4 |
| `qwen3-0.6b` | 0.6B | fail 100 s | fail 36 s | pass 37 s | pass 37 s | 2/4 |
| `qwen3-1.7b` | 1.7B | fail 134 s | timeout 300 s | pass 64 s | pass 50 s | 2/4 |
| `qwen3.5-0.8b` | 0.8B | pass 19 s | timeout 300 s | fail 153 s | pass 18 s | 2/4 |
| `minicpm5-1b` | 1.1B | pass 19 s | fail 35 s | fail 22 s | pass 26 s | 2/4 |

The columns are the four tests: `sum` is `reads_one_file_and_writes_another`, `fix` is
`fixes_a_failing_test_and_runs_it_again`, `summarize` is `a_plugin_asks_the_same_model_through_llm`,
`memory` is `remembers_across_sessions_through_the_memory_plugin`. A time is the test's wall
clock. **timeout** is nextest's `eval` profile killing the test at 300 s, which says the run
had not finished, not that it was wrong.

Apart from the eval defect below, the harness did not fail in any run: the logs of the `fix`
runs, which I read through, hold no plugin failure or stop and no model retry, and the only
`capability/denied` events (13) are `qwen3.5-0.8b` writing a workspace path wrong. The
failures are the models', with the two harness-side notes at the end.

## A defect in the evals, fixed

The first sweep, before any model was judged, showed LFM3 failing three of four, each time
with a final answer that claimed success. The work had
been done: the log showed `read`, `edit`, and `bash python3 test_calc.py` returning `ok`.

The cause is the command, not the model. Since `a04ce2d` the command writes progress (the
`▸` and `✓` lines) to standard error when standard output is not a terminal, so that a
program reading the answer reads nothing else
([the command's own docs](../crates/corduroy-cli/src/command.rs)). The evals' `run()` pipes
standard output, and `succeeded()` looks for the `▸ tool` lines in it, so it could never see
one. Three of the four evals (`fix`, `summarize`, and `memory`'s first half) could not pass
for any model. `run()` now passes `--progress stdout`; that is the whole change
(`1e18430`), and every number above is from after it. The first sweep's results were
discarded.

It is a case for the evals reading the command through its documented interface; one way to
keep it from happening again is a unit-level check that an eval's helper can see a
`▸ tool` line in what `run()` returns, which does not need a model.

## What each failure was

**`qwen3-0.6b`.**
- `fix`: made no tool call. One request, and an answer that the tools "do not include a
  function to run actual code execution" and a request for `calc.py`'s contents, though it
  has `bash` and `read`. It did not look.
- `sum`: read `fruit.txt` (`apples 3 …`) and then wrote `10+20+30=60` to `total.txt`: numbers
  that are not in the file, with the claim that the sum was done.

**`qwen3-1.7b`.**
- `fix` (300 s timeout, never finished): chose `delegate` first, then read a path it made up
  (`calculations/calc.py`), searched its own history three times with `recall_search`, and
  "fixed" the bug by `skill_manage patch` on a skill called `calc`, which does not exist. It
  never changed `calc.py`. The span was 297 s for three model requests; this model thinks
  before it answers, and on four CPU cores that is about a hundred seconds a request.
- `sum`: ran an `awk` that computed 10 into `total.txt`, then called `write` on the same file
  with the literal text `$sum`, overwriting the right answer with a shell variable name.

**`qwen3.5-0.8b`.**
- `fix` (300 s timeout, never finished): 28 `bash`, 13 `write` and 5 `read` calls, 14 of
  them failures, `calc.py` unchanged. The first dozen calls were refused by the `fs-read` and
  `fs-write` grants because the model wrote the workspace path wrong
  (`/tmp/corduroro-eval-fix-…` for `/tmp/corduroy-eval-fix-…`), the same slip the evals'
  `acme-site` comment says a small model makes. One call put the model's own tool-call markup
  into an argument (`"path": "</parameter>\n<parameter=depth>\n20"`), which is worth looking
  at in the chat template or the server's tool-call parser, since it is the model's
  format leaking, not a mistake in what it meant. It also overwrote `test_calc.py` with a
  script of its own, which the eval's check that the test is unchanged would have caught.
- `summarize`: the `summarize` tool's request to the same model was cut off by the add-on's
  `llm` wait (`the model did not answer within 120000ms`); the model then read the file itself
  and answered. The tool was reached and failed on time, not on use.

**`minicpm5-1b`.**
- `summarize`: said it had no `summarize` tool and asked for the file's contents. It was
  offered one.
- `fix`: asked for skills, then called `bash` with `"timeout": 0` twice. Each call came back
  `cell exceeded 0ms; the kernel was restarted and its state was lost`, and the model
  concluded it could not run anything and gave up with a list of questions. See below.

**`minicpm5-2b`.**
- `summarize`: the first call was `summarize {"paths": []}`, refused as invalid input; the
  model then read the file with `bash` and called `summarize {"paths": ["calc.py"]}`, which
  worked, and answered correctly. The eval failed it because `succeeded()` judges only the
  first call of a tool. By outcome this run is a pass; by the eval as written it is a fail,
  and the 3/4 above counts it as the eval does.

## Two things for the harness, not the models

1. **`bash` with `timeout: 0` is a zero-millisecond cell.** `shell-tool` takes the model's
   `timeout` as seconds and multiplies by 1000 with no floor
   ([`plugins/shell-tool/src/lib.rs`](../plugins/shell-tool/src/lib.rs)), so `0` kills the
   cell before it starts, and the message ("the kernel was restarted and its state was lost")
   says nothing about the timeout being the cause. A model that has just seen "default 120"
   and wants no limit may well write `0`. `minicpm5-1b` did, and it ended the run. Either
   refuse `0` as invalid input with a message that names the field, or read it as the default;
   whichever is chosen is a change to `bash`'s contract, which is why it is here and not in
   the commit.
2. **A tool that asks a model on the same CPU can outwait it.** `summarize` and `compact` use
   the `llm` capability, which waits at most `Limits::outbound_wait` (120 s by default). A
   0.8B model that thinks took longer than that, on a request the loop's own provider is
   allowed 300 s for (`timeout_seconds` in the agent). The two limits are separate, and the
   shorter one only produces a tool error. I have not checked whether the agent file can set
   the add-on's; if it cannot, a slow local model has no way to ask for longer.

## Reading across the models

- The two that passed everything are the two with the best tool discipline, not the biggest:
  `lfm3-1.7b` and `qwen3.5-2b` each made a short, relevant sequence of calls on `fix` (`read`,
  `edit`, `bash` to confirm) in 49 to 58 s. `minicpm5-2b` was close and did the same.
- `qwen3-0.6b` and `minicpm5-1b` fail by not trying: no call, or a refusal that a tool does
  not exist. `qwen3-1.7b` and `qwen3.5-0.8b` fail by trying a great deal and landing nowhere,
  which is the expensive way to fail on a CPU: they used the whole 300 s.
- `memory` passed for all seven, and `sum` for five. Those two use the fewest steps, which is
  most of what separates them from `fix`.
- Time scales with how much a model thinks, not with its size: `qwen3.5-0.8b` was the
  fastest on `memory` (18 s) and tied for fastest on `sum` (19 s), and the second slowest
  overall (490 s, after `qwen3-1.7b`'s 549 s).

## Run it again

```bash
scripts/local-model.sh setup               # builds llama-server once (~4 min), 1.5 GB
cargo plugins
for m in $(scripts/local-model.sh models | awk '{print $1}'); do   # or name them
  CORDUROY_LOCAL_MODEL=$m scripts/local-model.sh start
  cargo eval --no-fail-fast | tee "eval-$m.log"
  scripts/local-model.sh stop
done
```

A model's first start also fetches it, 0.5 to 1.8 GB. The seven took about 32 minutes in
all, 10 of them the two timeouts. A run of the same model again will not give the same
cells.
