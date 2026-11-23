# Stop, resume and recover runs

Every step of a run is in the log before anything else happens, so a run can be stopped,
abandoned, or picked up after a crash without losing its place.

## Stop a run that is going

Press **Ctrl-C** once. The running tool is told to stop (a shell command is interrupted),
calls not yet begun are answered as not run, and the run ends as `stopped` (exit status 130).
The session takes new prompts straight away.

Press Ctrl-C a second time to leave at once; the run is then unfinished (below).

## Pick up a run that was cut short

If the process was killed, the machine rebooted, or you pressed Ctrl-C twice:

```bash
corduroy continue
```

```text
  ↻ picking up a run that was cut short
```

Each tool call of the interrupted step is handled by how far the log says it got:

| The log has | Then |
| --- | --- |
| its result | the result is handed to the model; the tool is not run again |
| no start | it never ran; it runs now |
| a start but no result | it was running. It is run again only if the tool is `replay-safe`; otherwise the model gets an `interrupted` error and decides what to do |

Starting `corduroy` to chat, or `corduroy run` with a new prompt, also finishes an unfinished
run first. After five restarts of the same run, it ends as failed, so a crash loop does not
go on for ever.

## Abandon a run

```bash
corduroy stop
```

Ends the session's unfinished or waiting run without finishing it.

## Answer a question you left

A run that is waiting for your answer to a gate is not interrupted, and does not count as a
restart. `corduroy continue` asks the question again. See
[Decide what runs without asking](control-approval.md#answer-later).

## Work in several sessions

A session is one conversation. The default is `main`.

```bash
corduroy --session refactor run "plan the refactor of src/parser.rs"
```

```bash
corduroy sessions
```

In a chat, `/session refactor` switches (starting it if new) and `/sessions` lists them.
Sub-agents started with `delegate` are sessions too, listed with the session they came from.

## Start over

Sessions, runs, plugin state and the conversation all live in the log. To start from
nothing, move it away:

```bash
mv .corduroy/log.jsonl .corduroy/log.old.jsonl
```

Files the agent wrote, and what `skills` and `memory` keep in `.corduroy/`, are not in the
log and stay.
