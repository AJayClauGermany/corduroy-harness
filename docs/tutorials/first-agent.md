# Your first agent

In this tutorial you will build corduroy, point an agent at a folder, watch it work, approve
what it wants to change, leave one of its questions unanswered and come back to it later,
and read the log it keeps. It takes about fifteen minutes, most of it compiling.

You need a terminal on Linux or macOS (Windows works too, with PowerShell as the agent's
shell, but the commands below are for a POSIX shell), `git`, and
[rustup](https://rustup.rs). You also need a model: an Anthropic or OpenAI API key, or about
ten minutes more to run a small one on your own machine.

## 1. Build corduroy

Get the source and build the add-ons (the WebAssembly plugins) and the `corduroy` command:

```bash
git clone https://github.com/AJayClauGermany/corduroy-harness.git
```

```bash
cd corduroy-harness
```

```bash
cargo plugins
```

```bash
cargo build -p corduroy-cli
```

The first command after `cd` installs the Rust version the project pins, with its
WebAssembly target, before it builds anything; that is `rust-toolchain.toml` at work.

Put the command on your path for the rest of this tutorial:

```bash
export PATH="$PWD/target/debug:$PATH"
```

The command finds the add-ons you just built in this checkout, so leave it where it is.

To skip building, [install a ready-made build](../how-to/install-a-build.md) instead; it
puts `corduroy` on your path with its add-ons. You still want the checkout for
`scripts/local-model.sh` in the next step if you choose a model on your own machine.

## 2. Choose a model

corduroy picks the model from your environment when it writes an agent. Do **one** of these:

- **Anthropic:** `export ANTHROPIC_API_KEY=...`
- **OpenAI:** `export OPENAI_API_KEY=...`
- **A model on this machine, no key:** run `scripts/local-model.sh start` from the
  checkout (it downloads and builds about 1.5 GB the first time, pinned by checksum, and
  serves the model on `127.0.0.1:8080`). You will point the agent at it in step 4.

## 3. Make a workspace

The agent works in one folder, its **workspace**, and can touch nothing outside it. Make one
with a file in it:

```bash
mkdir -p ~/corduroy-tutorial && cd ~/corduroy-tutorial
```

```bash
printf 'buy milk\ncall Sam\nwater the plants\n' > todo.txt
```

## 4. Write the agent file

```bash
corduroy init
```

```text
wrote /home/you/corduroy-tutorial/corduroy.toml: edit it to change what the agent can do
put the key `anthropic` in the macOS Keychain:  corduroy secrets set anthropic
  ($ANTHROPIC_API_KEY is set here; this copies it:  printenv ANTHROPIC_API_KEY | corduroy secrets set anthropic)
then try it for real:  corduroy doctor
```

With a key from step 2, the agent reads it from your operating system's keychain, where it
is kept encrypted, and not from the environment. Run the command `init` printed to copy it
there, once:

```bash
printenv ANTHROPIC_API_KEY | corduroy secrets set anthropic
```

```text
kept `anthropic` in the macOS Keychain
```

(On a machine without a keychain, such as a container, `init` says so and writes an agent
that reads the variable; there is then nothing to copy. With the local model there is no
key at all.)

Open `corduroy.toml`. Everything this agent can do is in it: which model it asks, the shell
it runs commands in, and a list of `[[plugins]]`, each with exactly what it is
**granted**. Look at the first plugin, `pi-tools`, which gives the model its `read`, `write`,
`edit`, `bash` and `tree` tools:

```toml
[plugins.grants]
fs-read = ["{workspace}", "{workspace}/*"]
fs-write = { allow = ["{workspace}", "{workspace}/*"], deny = ["{workspace}/.git/*"] }
kernel = "shell"
```

It may read the workspace, write in it except `.git`, and run commands in the kernel named
`shell`. It can reach nothing else, whatever the model asks.

If you chose the local model in step 2, replace the `kind = ...` line under
`[providers.main]` with these three:

```toml
kind = "openai-compatible"
base_url = "http://127.0.0.1:8080/v1/chat/completions"
timeout_seconds = 300   # a small model on a CPU can take a while
```

The local server answers to any model name, so the rest of the file can stay as it is.

## 5. Check what the agent has

```bash
corduroy check
```

```text
model     claude-sonnet-5-5 from main
log       /home/you/corduroy-tutorial/.corduroy/log.jsonl
providers main
kernels   shell
gates     approval, inspect
points    plugin/load, tool/call
tool      bash
tool      delegate
tool      edit
tool      read (read-only)
…
tool      write
```

`check` loads every plugin without asking the model anything. Two plugins are **gates**:
`approval` and `inspect` are asked before every tool call (`tool/call` is the decision
point). Tools marked `read-only` said they change nothing, and `approval`, in its default
`smart` mode, lets those through without asking you.

## 6. Give it a task

```bash
corduroy run "How many items are in todo.txt? Write the number to count.txt."
```

The agent's tool calls are shown as they happen, one line each. A read goes straight
through. When it wants to write, `approval` stops it and asks you:

```text
▸ read {"path":"todo.txt"}
  ✓ buy milk (+2 lines)
▸ write {"path":"count.txt","content":"3\n"}
? write {"path":"count.txt","content":"3\n"}
  approval asks: `write` can change things
  allow? [y/N, or a reason to refuse]
```

Type `y` and press Enter. The write runs and the agent finishes:

```text
  ✓ Wrote 2 bytes to count.txt
There are 3 items in todo.txt; I wrote 3 to count.txt.
```

Your model will choose its own words and maybe other tools, so your output will differ in
the details. Check the result:

```bash
cat count.txt
```

If you type a reason instead of `y`, the call is refused and the model is told your reason,
so it can try something else.

## 7. Walk away from a question

Ask for another change, and this time, when it asks, press **Ctrl-D** instead of answering:

```bash
corduroy run "Add 'post the letter' to todo.txt."
```

```text
? edit {"path":"todo.txt", …}
  approval asks: `edit` can change things
  allow? [y/N, or a reason to refuse]
waiting for 1 answer(s); run `corduroy continue` to give them
```

The command has exited, but the run has not ended: it is **waiting**, and the wait is in the
log, not in the process. You could reboot the machine now. When you are ready:

```bash
corduroy continue
```

The same question comes back. Answer `y` and the run finishes where it left off. The same
command also picks up a run that was cut short by a crash or a killed process.

## 8. Read the log

Everything that happened is in `.corduroy/log.jsonl`. Print it:

```bash
corduroy log
```

Find your last run in it. Leaving out the plugins starting up, it reads something like:

```text
2026-11-23T10:15:33Z #49 main [user] message {"role":"user","parts":[{"type":"text","text":"Add 'post the letter' to todo.txt."}]}
2026-11-23T10:15:33Z #51 main [agent] tool/call (after #50) {"id":"c1","tool":"edit",…}
2026-11-23T10:15:33Z #53 main [host] gate/decision (after #51) {"point":"tool/call","verdict":"ask","reason":"`edit` can change things","gate":"approval",…}
2026-11-23T10:16:02Z #82 main [user] gate/answer (after #51) {"id":"c1","tool":"edit","allow":true,"reason":""}
2026-11-23T10:16:02Z #83 main [host] tool/start (after #51) {"id":"c1","tool":"edit"}
2026-11-23T10:16:02Z #84 main [environment] tool/result (after #51) {"id":"c1","tool":"edit","ok":true,…}
2026-11-23T10:16:03Z #88 main [host] turn/end {"status":"finished"}
```

Each line is one **event**: when it was stored, its number, its session (`main`), who it
came from in brackets, what caused it, its topic and its payload. The gate's decision was
stored before the tool could start, your answer is there with the time you gave it, and
the conversation the model sees is rebuilt from these events alone. Nothing about a run lives
anywhere else.

## 9. Talk to it

`corduroy` with no command is a conversation:

```bash
corduroy
```

Type a request, or `/tools` to see the tools, `/help` for the other commands, and `/exit` to
leave. Ctrl-C while the agent is working stops the run; the tool that was running is told to
stop.

## What you have done

You have run an agent whose every ability is a plugin listed in one file, seen a gate stop a
change until you agreed, left a run waiting across process lifetimes and resumed it, and read
the log that is the record of it all.

Next, [write an add-on of your own](first-add-on.md). Or, to change this agent:
[connect another model](../how-to/connect-a-model.md),
[change what runs without asking](../how-to/control-approval.md), or
[add an MCP server](../how-to/add-an-mcp-server.md).
