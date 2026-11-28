# The bench in pieces: results so far

What `piece.sh` has kept. `<model>.jsonl` is each model's results file (put it back with
`piece.sh restore`, which every piece does itself), `summary.md` the comparison and
`collected.jsonl` every record in one file. This page says what the records show so far; it
is notes on work under way, not a stock-take like
[`docs/local-model-findings.md`](../../../docs/local-model-findings.md).

## Where it stands (27 November 2026)

Three pieces, about 100 minutes of bench, in a four-core sandbox with two models served side
by side (two threads each, 6 to 8 tokens a second). Tier 1 only (one ability a task), **one
trial each**: read a failure as "it can fail", never as a rate.

| Model | Tier 1 so far | Failed |
| --- | --- | --- |
| `lfm3-1.7b` | 18/18 | |
| `minicpm5-2b` | 18/18 | |
| `qwen3.5-2b` | 13/14 (not finished) | `continuity` |
| `qwen3-1.7b` | 5/7 (not finished) | `bash` (empty answer), `find` (one request past 300 s) |
| `qwen3-0.6b` | 9/13 (not finished) | `edit`, `bash`, `find`, `hints` |
| `qwen3.5-0.8b` | 9/14 (not finished) | `chat`, `shell_state`, `find`, `grep`, `continuity` |
| `minicpm5-1b` | 4/18 | all but `chat`, `shell_state`, `today`, `mcp` |

Every model loaded and answered through the harness; none failed to be served. Nothing
above tier 1 has been run, nor tiers 0, 2 and 3, and no hosted model.

## To go on

```bash
scripts/model-bench/piece.sh 80 --models qwen3.5-2b,qwen3-1.7b,qwen3-0.6b,qwen3.5-0.8b --jobs 2 --tier 1 --trials 1
```

then commit `results/`. After that, the ladder for the models that hold tier 1
(`--tier 4 --climb 0.5`), and second and third trials (`--trials 3`) before any count here
is called a rate. A sandbox of about two hours has room for one 80-minute piece after the
builds and downloads (about 10 minutes).

## What the small models lack, so far

Below about one billion parameters the trouble is not hard tasks: it is the first step.
Each of these is from a record in `collected.jsonl` (the task is named).

1. **They say they have no tools.** With twenty tools offered, `minicpm5-1b` answered "I
   don't have the ability to modify config.ini files" (`edit`), "I don't have the capability
   to compute SHA-256" (`bash`), and wrote the todo list out as prose (`todo`), all without
   one call. `qwen3-0.6b` did the same on `find`. The same models call a tool readily when
   few are in play (`mcp`, `today` pass). Worth trying: fewer tools offered to a model that
   says it is small, or tools offered by need.
2. **The example in a description is taken for the answer.** The `path` parameters say
   "such as src/main.rs". Asked to write `hello.txt`, `minicpm5-1b` wrote to `src/main.rs`
   and reported success (`write`); in `read` it told of a file at `src/main.rs` that was
   never there. The example was added to stop absolute paths; for a 1B model it is worse
   than none. A placeholder that cannot be a real file would not be copied whole.
3. **The wrong tool, used with confidence.** `qwen3.5-0.8b` ran a shell command by calling
   `write` with the command as the file's name (`shell_state`: a file called
   `echo $BENCH_VALUE > kept.txt` now exists) and called `summarize` on a sum (`chat`).
   Both calls succeeded, so nothing told the model it had gone wrong.
4. **What is in the prompt is not read.** `hints` puts AGENTS.md in the prompt; `qwen3-0.6b`
   and `minicpm5-1b` both answered that they could not know the build command. `continuity`
   (the earlier turn is in the conversation) failed the same way on `minicpm5-1b`.
5. **A run ends as `finished` with nothing said.** `qwen3-0.6b` ran `sha256sum`, got the
   hash and ended with an empty answer (`bash`); `qwen3.5-0.8b` did so on `continuity`. The
   harness records these as finished, exit 0. An empty last message after a tool result
   could be its own ending, or be asked once more.
6. **Stuck runs go to the limit.** `qwen3.5-0.8b` spent 50 requests and 360 000 input
   tokens on `find`, and 50 again on `grep`, each a slightly different shell command;
   `qwen3-0.6b` spent 15 minutes on `edit` asking for `/var/tmp/config.ini`. `inspect`
   stops the same input four times in a row, which a model that varies a character walks
   past. Nothing looks at "no call has succeeded usefully for N steps".
7. **Absolute paths again, and the refusal's advice does not land.** The refusal now says
   where paths start from; `qwen3-0.6b` read it nine times and asked for
   `/var/tmp/config.ini` each time (`edit`).
8. **One request can take the whole timeout.** `minicpm5-1b` twice had a single request
   still going after 300 seconds (`grep`, `memory`), with no tokens recorded for it.

`lfm3-1.7b` and `minicpm5-2b` did none of these, and `qwen3.5-2b` one (4, on
`continuity`): at about two billion parameters tier 1 holds, and the line this list draws
is between one and two billion. `qwen3-1.7b`, which thinks before it answers, is the one of
that size that is slow enough at two threads to meet 5 and 8.

## What the bench itself got wrong

- **The model server grew until the sandbox killed it.** llama-server keeps up to 8 GB of
  earlier prompts by default; a 0.6 GB model reached 8.4 GB in twenty minutes and it and
  its neighbour were killed at the memory limit. `local-model.sh` now passes
  `--cache-ram` (1 GB; `CORDUROY_LOCAL_MODEL_CACHE`). The two trials that died with the
  servers were dropped and run again.
- **A trial cut off stays open to the others.** A finished trial's directory is closed to
  the agent's user; one that was running when the bench was stopped is not. `qwen3.5-0.8b`,
  looking for a `.cfg` file, went through `/var/tmp` and found `qwen3-1.7b`'s unfinished
  `find` workspace. It still failed, but with `--jobs` every model runs as the same user
  and could read a neighbour's workspace while it was in use. Now each model's agent is a
  user of its own, a trial's directory is that user's alone, and a trial left open is
  closed when the bench next starts.
- **Two threads a model is slow enough to matter.** At 6 to 8 tokens a second a thinking
  model takes one to two minutes for a one-call task; the 300-second request timeout and
  the 900-second command limit are nearer than in the stock-take. Compare only with runs
  made with the same `--jobs`.
