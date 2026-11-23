# corduroy-harness

A plugin-first agent harness in Rust. A small core provides a plugin tree, an event stream and
a programmable loop; **everything else is a plugin**: the tools a model can call, the text in
its system prompt, the kernels it computes in, and the reactions to what happens. A plugin
is anything you plug into the core and can unplug again, and comes in two kinds:
extensions and add-ons.

**New here?** Start with the [documentation](docs/README.md): a tutorial that gets an agent
running on a folder of yours, one that has you write an add-on, how-to guides, a full
reference of the command, the agent file and the capabilities, and the ideas behind it.

There are three tiers, from what changes least to what changes most:

| Tier | What | Gets in by | Trust |
| --- | --- | --- | --- |
| **Core** | The plugin tree, capabilities, the event log, the registries, the loop and the [plugin world](wit/corduroy.wit). The contract everything else is written against. | always there | the trusted base |
| **Extensions** | Native plugins that extend what the core can do: a kernel, an HTTP client, a model provider, MCP, a step of the loop, a decision point, a service for add-ons to call. | compiled into the command; a new one needs it rebuilt | the host's authority |
| **Add-ons** | WebAssembly plugins that add onto what is there: tools, prompt text, gates, automations. | a `.wasm` file, plugged in at start or while the agent runs; no rebuild | none beyond what they are granted |

Add-ons are **WebAssembly components** typed with [WIT](wit/corduroy.wit), so they can be
written in Rust, JavaScript/TypeScript or Python, each with an SDK
([`plugins/guest`](plugins/guest), [`sdk/js`](sdk/js), [`sdk/python`](sdk/python)), or any
other language that compiles to a component. An add-on starts with no authority at all and
is granted scoped **capabilities**, in the spirit of Deno's permissions and Tauri's
capabilities:

```rust
let runtime = WasmRuntime::new()?.with_root(&workspace);
harness.plugin(
    runtime
        .load("plugins/target/wasm32-unknown-unknown/release/pi_tools.wasm")?
        .grant("fs-read", Scope::subtree("/work"))          // may read below /work
        .grant("fs-write", Scope::subtree("/work/out"))     // may write below /work/out only
        .grant("kernel", Scope::none().allow("shell")),     // may use the kernel named `shell`
);
```

An add-on that imports an interface it was not granted is never instantiated; a call outside a
grant's scope is refused and recorded on the event stream.

Runs are **durable**. Each step of the loop is one atomic commit to a log, stored before
anyone sees it, and a run that was cut short is picked up from the log:

```rust
let events = Arc::new(corduroy_jsonl::open("run.jsonl")?);
let harness = Harness::resume(provider, events)?;       // sessions come back from the file
harness.continue_run(&MAIN.into()).await?;              // finish what a crash interrupted
```

Each tool call is logged when it starts and when it returns, so after a crash a finished
call's result is reused, a call that never started simply runs, and the one that was running
is repeated only if its tool says that is safe; otherwise the model is told and decides.

**Gates** decide whether a tool call may run. Any plugin can be one, add-ons included;
the strictest answer wins, and an "ask" makes the run wait, across restarts, for a person:

```rust
harness.plugin(runtime.load("plugins/…/approval.wasm")?.config(r#"{"mode":"smart"}"#));
if let Status::Waiting { asks, .. } = harness.run("tidy the repo").await?.status {
    harness.answer(&asks[0].session, &asks[0].call, Answer::Allow).await?;   // run the write
}
```

**Sub-agents** wait in the log too: `delegate` starts a child session with the same tools,
the parent's run stops until the child ends, and the child's answer becomes the call's
result. Any tool call can wait this way, for a signal brought by `Harness::signal`.

## Try it

```bash
cargo plugins                                   # build the add-ons (WebAssembly plugins)
```

```bash
export ANTHROPIC_API_KEY=...                    # or OPENAI_API_KEY; without either, a local Ollama
```

```bash
cargo corduroy --workspace ~/some/project      # talk to the agent; /help for commands
```

Without an agent file it runs the default agent: the Pi tools on a shell in the workspace,
skills, and an approval gate that asks before anything that writes. `cargo corduroy init`
writes that agent to `corduroy.toml` so you can change it: the providers, the kernels, each
add-on and exactly what it is granted. `corduroy run <prompt>` runs one prompt; `corduroy
check` says what the agent has without calling a model; `corduroy log` prints the event
log, with the time of each event, which is kept in `.corduroy/log.jsonl`. Ctrl-C stops a run (the running tool is told, the rest of its calls do not run); kill
the process instead and `corduroy continue` picks the run up, or `corduroy stop` ends it; leave a question unanswered and the run waits, in the log, until you do.

## Extending it

Write an **add-on** when the code should be sandboxed: anything from a third party, anything
the agent writes for itself, anything that only needs the capabilities in
[`wit/corduroy.wit`](wit/corduroy.wit). Use `plugins/guest` (Rust), `sdk/js` or
`sdk/python`, build it to a `.wasm` file, and list it under `[[plugins]]` in the agent file
with its grants; the agent can also load one while it runs (`plugin-manager`). Nothing but
the add-on is rebuilt.

Write an **extension** when the code needs the host's authority or a native library: a
kernel that runs in a container, a database behind a service, a step of the loop. An
extension is an ordinary native plugin (`corduroy_core::Plugin`), so everything it
registers goes away when it is unloaded. The agent file asks for one by kind:

```toml
[extensions.scratch]
kind = "shell-kernel"            # a kind this command was built with
dir = "{workspace}/scratch"      # the rest is the extension's own configuration
```

Kinds are compiled in, so a new kind means rebuilding the command. The command is a
library, so a command of your own is the shipped one with more kinds:

```rust
fn main() -> std::process::ExitCode {
    let extensions = Extensions::builtin().kind("audit", |asked| {
        let gates_point = asked.text("point").unwrap_or("audit/export").to_owned();
        Ok(Box::new(plugin_fn(asked.name, &[GATES], move |ctx| {
            // Declare a decision point of our own; WebAssembly gates may now guard it.
            ctx.service::<Gates>(GATES)?.declare(ctx, gates_point.clone(), "before an export")?;
            Ok(())
        })))
    });
    corduroy_cli::command::main_with(&extensions)
}
```

An extension can register kernels, HTTP clients, models and secrets (the things
capabilities reach), services that add-ons call through the `service`
capability, decision points for gates, and steps of the loop
(`corduroy_harness::Operations`). It cannot add a capability: the add-on boundary is the
core's. [`docs/design.md`](docs/design.md#three-tiers-core-extensions-add-ons) has the
reasoning, and [what is meant to stay stable](docs/design.md#what-is-stable).

## Where the ideas come from

| From | Idea | Where it lives |
| --- | --- | --- |
| [Cordis](https://github.com/cordiverse/cordis) (behind DeepSeek Harness) | Spatio-temporal composability: a plugin's effects revert when it unloads, and plugins follow the services they depend on | `corduroy-core::context` |
| [Prime Agent](https://github.com/PrimeIntellect-ai/prime-agent) | Long-lived kernels that keep state between tool calls | `corduroy-core::kernel` |
| [Hermes Agent](https://github.com/NousResearch/hermes-agent) | Skills the agent writes and repairs for itself | `plugins/skills` |
| [Pi](https://github.com/earendil-works/pi) | A minimal toolset: read, write, edit, bash | `plugins/pi-tools` |
| [OpenHands](https://github.com/OpenHands/software-agent-sdk) | An append-only event stream as the source of truth, and a loop you can program | `corduroy-core::event`, `src/session.rs` |
| [Goose](https://github.com/aaif-goose/goose) | A loop of steps, each reading the persisted conversation and returning effects; and its oi-kit providers, kept behind our own `Provider` | `src/harness.rs`, `crates/corduroy-providers` |

[Where the ideas come from](docs/explanation/inspirations.md) explains each idea and the
project it comes from, for a reader new to them; [`docs/design.md`](docs/design.md) has the
research behind each, what was borrowed, and what is deliberately left for later.

## Quick start

```bash
cargo plugins     # build the guest plugins to WebAssembly
```

```bash
cargo demo        # a scripted model works through both plugins; prints the event stream
```

```bash
scripts/guests.sh # build the JavaScript and Python plugins (needs node and python3)
```

```bash
cargo t           # fast tests via cargo-nextest
```

```bash
scripts/check.sh  # everything CI runs
```

```bash
scripts/local-model.sh start   # serve a small model on this machine (first time: ~4 min)
```

```bash
cargo eval                     # the agent does small real tasks with it, checked by outcome
```

Requires Rust 1.97+, the `wasm32-unknown-unknown` target (installed automatically from
`rust-toolchain.toml`) and [`cargo-nextest`](https://nexte.st)
(`cargo install cargo-nextest --locked`). Nothing needs a network or an LLM: the tests and the
demo drive the loop with a scripted provider. The evals are the exception: they run the agent
in [`examples/local-model/`](examples/local-model/corduroy.toml) against Liquid AI's LFM3
(1.7B parameters, 4-bit), served on `127.0.0.1` by llama.cpp through the ordinary
`openai-compatible` provider, so they need no API key and, once set up, no network.

## Layout

| Path | Purpose |
| --- | --- |
| `crates/corduroy-core` | Dependency-free core, `no_std` with an allocator (microcontrollers included; the default `std` feature adds what needs an OS): plugin contexts, capabilities, event stream with sessions and a `Log` trait, the registries plugins contribute to (tools, prompts, kernels, gates, services, models, secrets, HTTP clients) |
| `crates/corduroy-cli` | The `corduroy` command: an agent file (providers, kernels, secrets, extensions, plugins and their grants) run in a terminal; a library, so a binary can swap the standard extensions' parts or add extension kinds |
| `crates/corduroy-std` | The standard extensions for macOS, Windows and Linux, as one `Standard` whose parts (kinds, kernels, the log store, add-ons) can each be swapped |
| `crates/corduroy-mcp` | MCP servers (stdio, through a kernel, or streamable HTTP) as plugins |
| `crates/corduroy-providers` | Model providers from `oi-kit-providers` (Anthropic, OpenAI, Ollama and the providers Goose knows), converted to the harness's own messages at the boundary |
| `crates/corduroy-http` | The HTTP client behind the `http` capability, which never follows a redirect |
| `crates/corduroy-shell` | A persistent shell as a kernel: POSIX, or PowerShell on Windows |
| `crates/corduroy-jsonl` | A JSON Lines file as the durable log behind an event stream |
| `crates/corduroy-wasm` | wasmtime host that loads components as capability-gated plugins |
| `src/` | `corduroy-harness`: the agent loop (`no_std`, any executor; the loop itself swappable through the `Loops` registry), journaled to the event stream one commit per step; stopping a run; sessions, continuing an interrupted run, waiting for a person, and the model as a service |
| `wit/corduroy.wit` | The plugin world: host capabilities in, tools / event handlers / prompt text out |
| `sdk/js`, `sdk/python` | SDKs for plugins in JavaScript/TypeScript and Python, and the script host for automations written as scripts |
| `examples/` | The same `notes` plugin in JavaScript and Python |
| `plugins/guest` | `corduroy-guest`: write a plugin in Rust by implementing one trait |
| `plugins/pi-tools` | read, write, edit, bash on a persistent kernel, and tree |
| `plugins/skills` | Agent-written `SKILL.md` files with a prompt index and a review nudge |
| `plugins/approval` | A gate: permission modes with allow and deny lists |
| `plugins/{hints,todo,memory,recall,summarize,compact,subagents,inspect,plugin-manager}` | The rest of the Goose baseline: context files, a todo list, memories, searching earlier sessions, summaries by a second model, auto-compaction, sub-agents, a gate inspecting calls, loading plugins at run time |
| `plugins/testbed` | For the host's own tests: makes host calls, loads plugins, echoes events and traps on demand |
| `crates/corduroy-test-support` | Test-only helpers: finds the built plugins, makes scratch directories |
| `tests/`, `crates/*/tests/` | Integration tests through the public API |

See [`CLAUDE.md`](CLAUDE.md) for contributor and agent guidelines.

## License

[Boost Software License 1.0](LICENSE)
