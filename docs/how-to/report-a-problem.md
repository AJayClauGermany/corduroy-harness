# Report a problem

When a run does something you did not expect, the log already has what happened and when.
`corduroy report` puts the useful part of it in one piece to hand over, so nothing has to be
copied out of the terminal.

```sh
corduroy report > report.txt        # the last run of the session `main`
corduroy report 3 > report.txt      # its last three runs
corduroy --session side report      # another session's last run
```

It prints the version, what `corduroy check` says about the agent, and for each run how it
ended, what it waited on and for how long (each request to the model, each tool call, each
question you were asked), then every event with its time to the millisecond:

```text
run 2 of 2 in `main`: #24 to #26, 0.963s, failed Authentication error: … Status: 401 Unauthorized. …
written by {"program":"corduroy","version":"0.1.0","platform":"aarch64-macos",…}

waited on
   0.707s  model   anthropic/claude-… asked with 5 messages and 5 tools in `main`: no answer: the run ended as failed …

events (stored at, since the run began, since the event before)
2026-11-26T07:09:05.615Z +0.000s      0.000s  #24 main [user] message {"role":"user",…}
2026-11-26T07:09:05.871Z +0.256s      0.256s  #25 main [host] model/request {"provider":"anthropic",…}
2026-11-26T07:09:06.578Z +0.963s      0.707s  #26 main [host] turn/end {"status":"failed","reason":"…"}
```

A request to the model that never came back shows as `nothing in the log answers it`, with
the time it went out.

## What is in it

No key: the log never holds a secret's value, only its name. It does hold your prompts and
what the tools returned (cut at 1000 characters each in the report), so read it before you
send it.

## When the report is not enough

- `corduroy --session <name> log` prints every event of one session, whole.
- The log itself, `<workspace>/.corduroy/log.jsonl`, is plain text, one commit per line, and
  is the complete record. [Events](../reference/events.md) says how to read it.
- `corduroy check` alone tells whether the agent can be put together at all, and which
  add-on did not start.
