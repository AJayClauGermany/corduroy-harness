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
corduroy [options] log             print the event log, or with --session one session's part
corduroy [options] report [N]      what the session's last run (or last N) did, with timings
corduroy [options] doctor          try the agent for real: its secrets, kernels and models
corduroy init [vendor] [--secrets keychain|env]
                                   write the default agent file to edit
corduroy [options] secrets         list the agent's secrets and how each stands
corduroy [options] secrets set [name]
                                   put a secret's value in the keychain, typed without showing
corduroy [options] secrets remove [name]
                                   take it out again
corduroy [options] login [name]    sign in for a secret (a ChatGPT plan), or put its value in
corduroy [options] logout [name]   sign out again, or take the value out
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

**`log`** Print every event in the log, one per line, with the time it was stored (UTC, to
the millisecond). With `--session <name>`, only that session's events and those of the
sessions it started. See [Events](events.md) for how to read a line. The first lines each process writes are its
`process/start`: what program, version and platform wrote what follows.

**`report [N]`** Print what there is to hand to whoever looks into a problem, about the
session's last run, or its last `N`: the version, what `check` prints, and for each run

- how it ended and how long it took;
- what wrote it (the `process/start` before it);
- what it waited on, a line each with how long: every request to the model (provider,
  model, how many messages and tools, then the tokens and why it stopped, or the failure),
  every tool call, and every question a gate put to a person;
- every event stored while it went, with the time it was stored, the time since the run
  began and the time since the event before. Payloads are cut at 1000 characters; the log
  has them whole.

Under the run's first lines, `used` adds up what its model requests took: how many answered,
and the tokens in and out over all of them, the sessions it started included.

Nothing is asked of a model, and the keys are not in it: the log never holds a secret's
value. It does hold what you typed and what the tools returned, so read it before you send
it. `corduroy report > report.txt` keeps it as a file. Model requests are timed in runs
made by a build that records `model/request`; older runs show the rest.

**`doctor`** Try the agent for real, part by part, and say what works. Where `check` loads
the agent and asks nothing of the outside, `doctor` also

- sees whether each credential can be had, as `secrets` does;
- runs one command (`echo`) in each kernel;
- asks each model the agent names one short question, with how long the answer took and the
  tokens it used: the loop's model, and every model an enabled add-on's `llm` grant names
  outright (so a `reviewer` is tried too).

```text
✓ agent    /home/you/project/corduroy.toml: everything it lists started
✓ secret   anthropic (keychain, for provider main): in the macOS Keychain
✓ kernel   shell: ran a command in 9 ms
✗ model    main/claude-sonnet-5-5: the model failed: … Status: 401 Unauthorized. …
1 of the 4 parts tried is not working
```

It exits non-zero if anything failed. The questions cost a few tokens each; nothing of them
is a run, so the log gains only the `process/start` every command writes. A failure that is
a ChatGPT plan's is followed by what to do about it, as after a run.

**`init [vendor] [--secrets keychain|env]`** Write the default agent to
`<workspace>/corduroy.toml`, and say what to do next. Refuses if the file exists.

- *Whose model.* `vendor` is `anthropic`, `openai`, `chatgpt` (OpenAI's models, paid by a
  [ChatGPT plan](../how-to/connect-a-model.md#a-chatgpt-plan-instead-of-an-api-key)) or
  `ollama`. Left out, it is chosen from the environment: Anthropic if `ANTHROPIC_API_KEY` is
  set, else OpenAI if `OPENAI_API_KEY` is set, else a local Ollama. With both keys set, it
  also writes a second provider, `reviewer`, so the `review` add-on asks the other vendor's
  model.
- *Where its keys come from.* Each provider names a `[secrets.<name>]` entry (`anthropic`,
  `openai`, `chatgpt`). The entry is of `kind = "keychain"` where this machine has a
  keychain, and reads the environment variable (`env = "ANTHROPIC_API_KEY"`) where it has
  none: a server nobody is logged in to, a container. `--secrets keychain` or `--secrets env`
  says which; the keychain asked for on a machine without one is refused, and nothing is
  written in its place. A ChatGPT sign-in is kept in the keychain, or without one in a file
  of your own.

`init` writes where a key comes from, never the key. It prints the command that puts it
there (`corduroy secrets set anthropic`, or `corduroy login chatgpt`) and, when the vendor's
variable is set where you ran it, a line that copies it in. The agent used when there is no
agent file is unchanged: it names no secret, so its provider reads its own variable.

**`secrets`** List every credential the agent names and how it stands, without reading one
out: each `[secrets.<name>]` entry with its kind and what uses it, then each provider that
names no secret and reads a variable of the environment.

```text
✓ anthropic (keychain, for provider main): in the macOS Keychain
✓ chatgpt (chatgpt, for provider plan): signed in as you@example.com, kept in the macOS Keychain; its token is good for 52 more minutes
✗ github (env, for MCP server github): $GITHUB_TOKEN is not set
✓ provider reviewer (no secret named): reads $OPENAI_API_KEY from the environment, which is set
```

It exits non-zero if one cannot be had, and each such line says what to do. The agent is
not put together for this. On macOS, looking at an item may ask you to let `corduroy` use
it, as using it does.

**`secrets set [name]`** Put the value of a [`kind = "keychain"`](agent-file.md#secretsname)
secret into the system's keychain. It asks for the value and does not show it as you type
or paste; the spaces and line end around it are dropped. With the value piped in, it reads
one line and asks nothing, which is how a key is moved out of the environment:

```bash
printenv ANTHROPIC_API_KEY | corduroy secrets set anthropic
```

The value goes from you to the keychain: it is in no agent file, no log, and nothing the
command prints. `name` may be left out where the file has one such secret. A secret of
another kind is refused, with where its value goes instead. The keychain is your own, not
the workspace's: every agent of yours that names a secret alike reads the same item
(service `corduroy`, account the secret's name).

**`secrets remove [name]`** Take that value out of the keychain, and say whether there was
one.

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

**`login [name]`** Do what a secret of the agent file needs from you. For a
`kind = "keychain"` secret that is its value: `login` is then `secrets set`, and `logout` is
`secrets remove`. For a secret that is signed in to, a
[`kind = "chatgpt"`](agent-file.md#secretsname) secret, which is a ChatGPT plan, it opens
the browser at OpenAI's sign-in ("Continue with ChatGPT"), prints the address in case it
does not open, and waits up to five minutes for you to come back to it; the answer arrives
on `127.0.0.1` (port 1455, or another if that one is taken). The first sign-in registers
this tool for your account; later ones reuse that registration. What is kept is in your own
configuration directory (see [Files](#files)), or in the keychain where the entry says
`keep = "keychain"`, so every workspace whose agent names a secret of the same name shares
the sign-in. An entry that newly says `keep = "keychain"` while a sign-in is still in the
file has it moved, without the browser. `name` says which secret, where the file has
several. The agent is not put together for this.

**`logout [name]`** Revoke that sign-in with OpenAI and remove what was kept.

## Files

| Path | What |
| --- | --- |
| `<workspace>/corduroy.toml` | the agent file, unless `--agent` names another |
| `<workspace>/.corduroy/log.jsonl` | the event log (the agent file's `log` moves it) |
| `<workspace>/.corduroy/cache/` | compiled add-ons, so a second start is fast |
| `<workspace>/.corduroy/skills/`, `memory/` | what the default agent's `skills` and `memory` add-ons keep |
| `~/.config/corduroy/` (`$XDG_CONFIG_HOME/corduroy`; on Windows `%APPDATA%\corduroy`) | what is yours whatever you work on: `chatgpt/<secret>.json`, a ChatGPT sign-in (readable by you alone) unless its entry keeps it in the keychain, and `host-id`, a random name for this computer that the sign-in sends |
| your keychain, service `corduroy` | the values of `kind = "keychain"` secrets, each under its secret's name, and the ChatGPT sign-ins kept there, as `chatgpt-sign-in.<secret>`: in the macOS Keychain (Keychain Access shows them), the Windows Credential Manager (as `<name>.corduroy`) or the Secret Service on Linux (`secret-tool search service corduroy`) |

The agent file, the log, the cache and your own configuration directory are out of reach of
every add-on's `fs-read` and `fs-write`, whatever its grants say.

## Environment

| Variable | Used for |
| --- | --- |
| `ANTHROPIC_API_KEY`, `OPENAI_API_KEY` | the keys of providers that name no `secret`, as the agent with no file does, and `init`'s choice of model |
| `CORDUROY_PLUGIN_PATH` | more directories to find add-ons in, separated as `PATH` is, searched before the `plugins` directory beside the program |
| whatever `api_key_env`, `[secrets.*] env` and `bearer_token_env` name | keys and secrets the agent file refers to |
