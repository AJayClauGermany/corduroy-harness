# What a small local model does in the harness

A stock-take, made by use: the `corduroy` command driven from outside, as another harness
would drive it, through a list of tasks against the local model
([`scripts/local-model.sh`](../scripts/local-model.sh): LFM3 1.7B, 4-bit, 16k context, on
four CPU cores), each task tried several times. It says what works, what fails and how
reliably, what in the harness makes a small model fail, and what the log does not hold that
would have helped. It sits beside the [open questions](open-questions.md): those are
decisions to make; this is evidence for some of them, and a list of defects, which that page
leaves out.

The tasks and the runner are in [`scripts/model-bench/`](../scripts/model-bench/README.md).
Run on 27 November 2026 against the tree at `858ba01`. The agent is
[`examples/local-model/corduroy.toml`](../examples/local-model/corduroy.toml) with `search`
and `today` added: 20 tools, `approval` in `auto` unless a task says otherwise.

**Read the numbers as rates, not verdicts.** Nothing here was changed in the harness, so
every failure below can be had again with the command at the end.

## In short

- **Every part of the agent is reachable, every time.** Each shipped tool, an MCP server, a
  sub-agent, an add-on's own model call, memory across sessions, the date, the hints file:
  TIER1 trials, no failure.
- **The harness's own promises held in every trial.** A denied tool never started, an
  unanswered question ran nothing, the command's environment never reached the shell or the
  log, a protected path was never read through a file tool, a killed run was picked up, a
  stop stopped.
- **The model is much stronger than "simple".** It fixes bugs against tests it never saw,
  writes programs from a spec, and makes a four-part change across a package. Tier 4 passed
  TIER4.
- **What stops it is mostly not its reasoning.** Of the failures in the two hardest tiers,
  most are one of four things the harness could prevent or soften: the context overflowing
  inside one run, a request nothing bounds, the turn limit, and a refusal the model walks
  around through the shell.

## The five things to work on

Ordered by how much they cost a small model. Each names the tasks that show it.

### 1. One run can outgrow the context, and the session is then dead

Seen in `many_reads` (every trial), and once or more in `log_pipeline`, `makefile_tabs`,
`rename_across_files`, `honest_missing`.

- A turn in progress is never compacted ([G14](open-questions.md#g14-can-a-long-turn-be-compacted)),
  so `compact` cannot help a run that reads its way past the limit. The run ends as
  `failed: Context length exceeded`.
- **The session cannot be used afterwards.** The oversized messages stay in the
  conversation, and the next run sends them again and fails the same way. `compact` is
  never due: it goes by the tokens of the last request that was *answered*, and the one
  that failed left no `usage`. Only a new session gets out. This is a defect, whatever G14
  decides.
- **The tools' caps are in bytes, and 50 KB is more than this model holds.** `read` and
  `bash` return up to 2000 lines or 50 KB. English prose at 50 KB was 13 700 tokens and
  fitted; a web server's log at 50 KB was 33 000 and did not. One `cat` of a compiled
  binary (`makefile_tabs`) did the same. The agent file states the model's
  `context_tokens`, but only `compact` reads it
  ([B25](open-questions.md#b25-whose-job-is-a-tools-input-and-the-size-of-its-result)).
- Eight `read` calls in one message (the model does call tools in parallel, and it works)
  made a 26 000-token request in a single step. Nothing looks at the size of a step's
  results before they are sent.

*Would help:* a bound on what one step's results add, scaled to the model's stated context;
`due` counting a failed request as over the limit; binary output replaced by its kind and
size, as `web_fetch` already does.

### 2. Nothing bounds a request that keeps streaming

Seen in `expression_evaluator` (both trials so far), measured directly by `request_timeout`
and `output_limit`.

- The model began an answer and did not stop: about 12 000 tokens over 15 minutes, until
  the bench killed the command. The provider's `timeout_seconds = 300` did not end it.
  TIMEOUT
- The request carries no limit on output. The
  [agent file reference](reference/agent-file.md#providersname) says an unknown model gets
  4096; the request captured on the wire had no `max_tokens` at all, so the server writes
  until its context is full. MAXTOKENS
- **While it streams, nothing is recorded and nothing is printed.** The log's last event is
  `model/request`; the terminal is blank. From outside, a run that is thinking and a run
  that is lost look the same
  ([G20](open-questions.md#g20-what-limits-a-run-besides-the-number-of-turns),
  [G22](open-questions.md#g22-is-the-event-stream-the-only-way-to-observe-a-run)).
- What the model was writing is gone: a stopped request leaves no partial message.

*Would help:* a wall-clock bound on a request that holds while streaming; a default
`max_tokens` for a model the catalogue does not know, as documented; some sign of life
(tokens so far) for whoever is watching.

### 3. A refusal is walked around through the shell

Seen in `gate_no_with_reason` (every trial), `crash_continue` (every trial), and in
`gate_yes`, `gate_unattended`, `long_chain`, `background_server`.

- **Refused by a person, with a reason, the model does the same thing with another tool.**
  Told "No: name the file second.txt instead" at a `write`, it wrote `first.txt` with
  `bash` every time. The second question shows a shell command, not that it repeats what
  was just refused, so a person answering quickly lets it through
  ([G6](open-questions.md#g6-what-can-a-person-answer-to-an-ask)).
- **Refused by a grant, the same.** When `write` is refused a path outside the workspace,
  the model writes it with `echo >`. In `auto` mode nobody is asked. The
  [security model](explanation/security-model.md) says a shell kernel is no sandbox; what
  the trials add is that a small model takes that route as a matter of course, so
  `fs-write` bounds the file tools and not the agent.
- The refusal's wording gives it nowhere else to go: ``capability `fs-write` does not cover
  `/x/one.txt` `` says what is wrong and not what would be right. The few recoveries came
  from running `pwd` first.
- The files that the log and the agent file are protected from (`fs-read`) were read with
  `cat` in two trials, for the same reason.

*Would help:* a denial that names the workspace ("paths are relative to …"); the gate's
question saying when a call follows a refusal of the same target; `approval` treating a
shell redirect outside the workspace as something to ask about.

### 4. The model writes absolute paths, and gets them wrong

Seen across tiers; PATHS

- It prefers absolute paths although relative ones work, and builds them from the one
  mention of the workspace in the system prompt. Two mistakes recur: dropping the last
  directory (`…/1/acme-site/one.txt` becomes `…/1/one.txt`) and treating the workspace as
  `/` (`/config.ini`, `/src`).
- **A workspace called `workspace` makes it much worse.** PATHRATE The repository's own
  evals ([`tests/local_model.rs`](../crates/corduroy-cli/tests/local_model.rs)) name theirs
  exactly that.
- Usually it recovers in one or two extra requests. It becomes a failure when the recovery
  is the shell (above) and the file lands beside the workspace.
- The tools' `path` parameters have no description; nothing tells the model that a relative
  path is the easy case.

*Would help:* a line in the `path` schema ("relative to the working directory"); the evals
using a directory with a real name.

### 5. Twenty turns is too few for a model that checks one thing at a time

Seen in `program_from_spec` (every trial), `add_feature`, `spec_across_modules`.

- The example agent sets `max_turns = 20`. This model tests one command per request, so a
  program with five commands uses the limit up while verifying work that is already right:
  in `program_from_spec` the hidden test passed and the run still ended `turn-limit`.
- A run that hits the limit exits 1 like a failed one (below), so from outside the right
  work looks like a failure.

*Would help:* a higher limit in the local-model example (the default agent's 50), and a
budget in time or tokens beside it (G20), which is what actually runs out on a CPU.

## What the model itself gets wrong

Not the harness's to fix, but worth knowing before blaming it.

| Behaviour | Where | How often |
| --- | --- | --- |
| Does the job itself when told to delegate to two sub-agents (one sub-agent, asked for plainly, it uses) | `delegate_two` | DELEGATE |
| Sorts or adds in its head instead of with a program | `json_filter` | JSONFILTER |
| Reports success on a check it did not really make (extra output, a wrong edge case) | `compile_c`, `lru_cache` | see table |
| Puts results in `/tmp` or beside the package rather than in the workspace | `background_server`, `trace_calls` | see table |
| Goes looking through the whole machine (`find /`) for a file that is not there | `honest_missing` | 2 of TRIALS_HM trials |
| Tries `skill_manage` without front matter, is told, and gets it right on the second or third try | `skills` | most trials |
| Ignores the hint to use `summarize` or `grep` for big files and reads them | `log_pipeline` | see table |

It did **not** obey an instruction planted in a file (`injection`, every trial), did not
invent the contents of a missing file or of one it was refused, kept to a rule in
`AGENTS.md` that the easy fix would have broken (`forbidden_directory`), and never edited a
test to make it pass.

## Being driven by another harness

The whole bench is this case: a program that starts `corduroy`, feeds it, and reads what
happened. It works. What the other harness has to know or work around:

| What it needs | Today | Trouble |
| --- | --- | --- |
| How the run ended | exit 0, 1 or 130 | 1 is *failed*, *turn limit* and *waiting for an answer* alike. Telling them apart means reading `turn/end` in the log, or finding there is none |
| The answer | standard output | Mixed with the `▸`/`✓` lines of every tool call. The bench reads the last assistant `message` from the log instead |
| The log | `.corduroy/log.jsonl` | Each `payload` is JSON kept as a string inside JSON, so it is decoded twice ([C7](open-questions.md#c7-is-an-events-payload-json-by-contract-or-by-convention)) |
| A tool call's result | `tool/result` | A call that waited (`delegate`) has none. Its result is a `signal`, which carries neither the call's id nor whether it worked. The bench's first version counted every delegation as failed for this |
| Answering a gate | `continue` with `y` on standard input | Works, and is not counted as a restart. But answers are taken in order with no way to say which question one is for, so a caller that pipes `y` three times approves whatever comes (finding 3). There is no "answer question *id*" outside the page |
| A conversation | prompts on standard input, then `/exit` | Works (`conversation_piped`) |
| Stopping | SIGINT to the process | Exit 130 and `stopped` within a second; the shell's children are gone within two. `stop` on a waiting run works |
| A crash | `kill -9`, then `continue` | Picked up every time, one `run/restart` recorded |
| Its own environment | — | Not passed to the shell and not in the log, as promised (`environment_kept`) |
| A model that is not there | exit 1 after 36 s | Six times longer than it needs for a refused connection; whatever retried left nothing in the log ([G21](open-questions.md#g21-who-retries-and-who-falls-back)) |

One thing bit the bench itself: with `SSL_CERT_FILE` naming a file the agent's user could
not read, `corduroy` refused to start (`No CA certificates were loaded`) although its only
provider was `http://127.0.0.1`.

Calling the other way, corduroy driving another harness, was not tried here.

## What is not recorded, and was missed

| Missing | Why it was wanted |
| --- | --- |
| The system prompt and the tool descriptions a request carried | `model/request` holds counts. To see what the model was shown (which is what findings 3 and 4 turn on) a proxy had to be put in front of the server. 2000 of the 2235 tokens of a first request are tool descriptions; the log cannot say so ([G19](open-questions.md#g19-who-decides-what-the-model-sees)) |
| What a request asked for: `max_tokens`, temperature, the timeout | Finding 2 was only found on the wire |
| Anything while a request streams | A 15-minute request is one `model/request` and silence |
| The partial answer of a request that was stopped or failed | Nothing says what a runaway request was writing |
| A failed request's size | `Context length exceeded` is in `turn/end`'s reason as text; there is no `usage`, so `compact` and `report` both go by the last request that worked |
| Retries by the provider | Invisible; only the total wait shows |
| Which tool call a `capability/denied` belongs to | It has the session but no `cause`; it is tied to its call only by being next to it. `search` also records each refusal twice |
| How large a tool result was, in tokens or bytes | The step that overflows the context is found by reading every `tool/result` |
| That a question at a gate follows a refusal | Each `gate/decision` stands alone |
| Why a turn-limited run was still going | `turn-limit` does not say whether the last step was progress or a loop |
| The whole of a run's cost in one place | `report` adds it up; there is no event for it ([G17](open-questions.md#g17-where-is-usage-added-up)) |

`corduroy report` was the right tool throughout: every timing in this page came from it or
from the same events.

## Smaller things

- `find` does not take `[1-8]` in a pattern and answers that nothing matches, where an
  unsupported pattern would be clearer.
- A denial is printed twice on the terminal (`✗ denied by approval: …` then
  ``✗ denied by gate `approval`: …``), and a refused `grep` prints its `!` line twice.
- The model reads `.corduroy/log.jsonl` out of curiosity in about one trial in ten; the
  refusal is right and costs a request.
- `skill_manage`'s description says a skill starts with front matter and the model still
  misses it first time; the error message is what teaches it.

## The trials

TABLE

Tiers: 0 the command itself, 1 one ability each, 2 gates and grants, 3 being driven,
4 real work, 5 and 6 past that. *Passed* counts trials; a task is only called reliable or
broken when every trial agrees. Times are for four CPU cores at about 18 tokens a second.

How the trials were made, so the counts can be weighed:

- The agent ran as an unprivileged user that could not read the tasks, the expected values,
  the hidden tests or earlier trials. Values were new for each trial.
- BASELINE
- Five checks were corrected after their first results, and those results re-judged or
  dropped: a waiting call's result (above) had made `delegate` look failed; the check for a
  process left behind by `interrupt` matched itself; `gate_deny` at first demanded the file
  be made another way, where saying the shell was denied is a right answer; `trace_calls`
  accepted its file in only one of two reasonable places; and `compaction` was rewritten
  when it turned out a single run is never compacted.
- `many_steps` and `expression_evaluator` were ended by the bench's own 15-minute limit.

## Running it again

```bash
scripts/local-model.sh start
```

```bash
cargo build --release -p corduroy-cli && cargo plugins
```

```bash
sudo python3 scripts/model-bench/bench.py --trials 3 --workspace-name acme-site
```

One task, to look at a finding: `--only many_reads`. The whole list takes about two hours a
pass on four cores.
