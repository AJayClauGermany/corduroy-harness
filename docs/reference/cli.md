# The `corduroy` command

```text
corduroy [options]                 talk to the agent
corduroy [options] run <prompt>    one prompt, then exit
corduroy [options] continue        finish a run that was cut short or waits for an answer
corduroy [options] stop            end a run that was cut short or waits, without finishing it
corduroy [options] check           load the agent and say what it has, without a model
corduroy [options] sessions        list the sessions in the log
corduroy [options] sessions remove <name>
                                   take a session, and those it started, out of the log
corduroy [options] log             print the event log
corduroy init                      write the default agent file to edit
```

From a checkout, `cargo corduroy <args>` runs the command built from that tree (the alias
already ends in `--`). Source:
[`crates/corduroy-cli/src/command.rs`](../../crates/corduroy-cli/src/command.rs).

## Options

| Option | Default | Meaning |
| --- | --- | --- |
| `--workspace <dir>` | the current directory | Where the agent works. `{workspace}` in the agent file, and where `.corduroy/` is kept. |
| `--agent <file>` | `<workspace>/corduroy.toml`, else the default agent | The [agent file](agent-file.md) to read. |
| `--session <name>` | `main` | The session to run in. It is started if the log does not have it. |
| `--model <name>` | the file's `model` | Ask this model instead. |
| `--provider <name>` | the file's `provider` | Ask this provider instead; it must be one of the file's `[providers.*]`. |
| `-h`, `--help` | | Print the usage line. |

## Commands

**(none)** Talk to the agent. First finishes the session's run if one was left unfinished,
then reads prompts until end of input or `/exit`. Typed commands:

| Typed | Does |
| --- | --- |
| `/help` | list these |
| `/tools` | list the tools, with the first line of each description |
| `/sessions` | list the sessions, the current one starred |
| `/session <name>` | switch to a session, starting it (as a child of the current one) if it is new |
| `/exit`, `/quit`, Ctrl-D | leave |

**`run <prompt>`** Run one prompt to its end. The words after `run` are the prompt. If the
session has an unfinished run, that is finished first.

**`continue`** Pick up the session's unfinished run: one cut short by a crash or a killed
process (each time counted as a restart; past five the run fails), or one waiting for your
answer to a gate, which is asked again.

**`stop`** End the session's unfinished run without finishing it. Says whether there was one.

**`check`** Load the agent, every plugin included, and print its model, log, providers,
kernels, gates, decision points, extra loop steps and tools, without calling a model. Lists
plugins that did not start, and exits non-zero if any did not.

**`sessions`** List the sessions in the log, each with the session it was started from.

**`sessions remove <name>`** Take the session out of the log, with every session started from
it (sub-agents and sessions switched to from it), and say which went. The log is written
again without their events and with a `session/removed` marker for each, then put in place of
the old one in one step, so a crash leaves one or the other. Nothing else in the log changes:
plugin state, other sessions and the events about the agent itself stay. The agent is not put
together for this, and no other `corduroy` may have the log open. It cannot be undone.

**`log`** Print every event in the log, one per line, with the time it was stored (UTC).
See [Events](events.md) for how to read a line. The first lines each process writes are its
`process/start`: what program, version and platform wrote what follows.

**`init`** Write the default agent to `<workspace>/corduroy.toml`. Refuses if the file
exists. The model is chosen from the environment: Anthropic if `ANTHROPIC_API_KEY` is set,
else OpenAI if `OPENAI_API_KEY` is set, else a local Ollama.

## What is shown while a run goes

| Line | Means |
| --- | --- |
| text | what the model said |
| `▸ tool {input}` | the model called a tool |
| `  ✓ output` / `  ✗ output` | the tool returned / failed (the model sees the full text) |
| `? tool {input}` then `allow? [y/N, or a reason to refuse]` | a gate asks you. `y` runs the call; Enter or `n` refuses it; any other text refuses it and is the reason the model is given; Ctrl-D leaves the run waiting |
| `  ✗ denied by <gate>: reason` | a gate refused the call |
| `  ! <plugin> was refused <capability> on <resource>` | an add-on asked for something its grant does not reach |
| `  [plugin:<name>] message` | an add-on wrote to its log |
| `  ! plugin <name> failed/stopped: reason` | an add-on could not start, or trapped too often |
| `  ↻ picking up a run that was cut short` | a restart |
| `  [<session>] …` | a line from a sub-agent's session |

**Ctrl-C** while a run goes asks it to stop: the running tool is told, calls not yet begun do
not run, and the run ends as stopped. A second Ctrl-C leaves at once, and the run is
unfinished until `continue` or `stop`.

## Exit codes

| Code | When |
| --- | --- |
| 0 | the run finished, or the command did what it was asked |
| 1 | the run failed, reached the turn limit, or is waiting; `check` found a plugin that did not start; any error |
| 130 | the run was stopped (as after Ctrl-C) |

## Files

| Path | What |
| --- | --- |
| `<workspace>/corduroy.toml` | the agent file, unless `--agent` names another |
| `<workspace>/.corduroy/log.jsonl` | the event log (the agent file's `log` moves it) |
| `<workspace>/.corduroy/cache/` | compiled add-ons, so a second start is fast |
| `<workspace>/.corduroy/skills/`, `memory/` | what the default agent's `skills` and `memory` add-ons keep |

The agent file, the log and the cache are out of reach of every add-on's `fs-read` and
`fs-write`, whatever its grants say.

## Environment

| Variable | Used for |
| --- | --- |
| `ANTHROPIC_API_KEY`, `OPENAI_API_KEY` | the default providers' keys, and `init`'s choice of model |
| `CORDUROY_PLUGIN_PATH` | more directories to find add-ons in, separated as `PATH` is |
| whatever `api_key_env`, `[secrets.*] env` and `bearer_token_env` name | keys and secrets the agent file refers to |
