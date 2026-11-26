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
corduroy [options] login [name]    sign in for a secret that needs it (a ChatGPT plan)
corduroy [options] logout [name]   sign out again
corduroy --version                 print the version (and commit) of this build
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
| `-V`, `--version` | | Print the version, and the commit a [ready-made build](../how-to/install-a-build.md) was made from. |

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
else OpenAI if `OPENAI_API_KEY` is set, else a local Ollama. With both keys set, it also writes a second
provider, `reviewer`, so the `review` add-on asks OpenAI's model about Anthropic's work.

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
| `Using ChatGPT plan. Manage usage: <address>` | before a run, when the model's provider is paid by a [ChatGPT sign-in](agent-file.md#secretsname): whose account the run is on, and where to see and limit what it uses |
| a line after `the run failed: …` | when such a run fails for a reason that is the plan's (its usage limit, a sign-in no longer accepted, something a plan does not pay for): what you can do about it. Where OpenAI's reason carries no code, the line is a guess and says "if" |

**Ctrl-C** while a run goes asks it to stop: the running tool is told, calls not yet begun do
not run, and the run ends as stopped. A second Ctrl-C leaves at once, and the run is
unfinished until `continue` or `stop`.

## Exit codes

| Code | When |
| --- | --- |
| 0 | the run finished, or the command did what it was asked |
| 1 | the run failed, reached the turn limit, or is waiting; `check` found a plugin that did not start; any error |
| 130 | the run was stopped (as after Ctrl-C) |

**`login [name]`** Sign in for a secret of the agent file whose kind is signed in to: a
[`kind = "chatgpt"`](agent-file.md#secretsname) secret, which is a ChatGPT plan. It opens
the browser at OpenAI's sign-in ("Continue with ChatGPT"), prints the address in case it
does not open, and waits up to five minutes for you to come back to it; the answer arrives
on `127.0.0.1` (port 1455, or another if that one is taken). The first sign-in registers
this tool for your account; later ones reuse that registration. What is kept is in your own
configuration directory (see [Files](#files)), so every workspace whose agent names a secret
of the same name shares the sign-in. `name` says which secret, where the file has several.
The agent is not put together for this.

**`logout [name]`** Revoke that sign-in with OpenAI and remove what was kept.

## Files

| Path | What |
| --- | --- |
| `<workspace>/corduroy.toml` | the agent file, unless `--agent` names another |
| `<workspace>/.corduroy/log.jsonl` | the event log (the agent file's `log` moves it) |
| `<workspace>/.corduroy/cache/` | compiled add-ons, so a second start is fast |
| `<workspace>/.corduroy/skills/`, `memory/` | what the default agent's `skills` and `memory` add-ons keep |
| `~/.config/corduroy/` (`$XDG_CONFIG_HOME/corduroy`; on Windows `%APPDATA%\corduroy`) | what is yours whatever you work on: `chatgpt/<secret>.json`, a ChatGPT sign-in (readable by you alone), and `host-id`, a random name for this computer that the sign-in sends |

The agent file, the log, the cache and your own configuration directory are out of reach of
every add-on's `fs-read` and `fs-write`, whatever its grants say.

## Environment

| Variable | Used for |
| --- | --- |
| `ANTHROPIC_API_KEY`, `OPENAI_API_KEY` | the default providers' keys, and `init`'s choice of model |
| `CORDUROY_PLUGIN_PATH` | more directories to find add-ons in, separated as `PATH` is, searched before the `plugins` directory beside the program |
| whatever `api_key_env`, `[secrets.*] env` and `bearer_token_env` name | keys and secrets the agent file refers to |
