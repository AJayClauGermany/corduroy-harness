# corduroy-harness

A plugin-first agent harness in Rust. A small core provides a plugin tree, an event stream and
a programmable loop; **everything else is a plugin**: the tools a model can call, the text in
its system prompt, the kernels it computes in, and the reactions to what happens.

Plugins are native Rust or **WebAssembly components** typed with [WIT](wit/corduroy.wit), so
they can be written in Rust, JavaScript/TypeScript or Python, each with an SDK
([`plugins/guest`](plugins/guest), [`sdk/js`](sdk/js), [`sdk/python`](sdk/python)), or any
other language that compiles to a component. A WebAssembly plugin starts
with no authority at all and is granted scoped **capabilities**, in the spirit of Deno's
permissions and Tauri's capabilities:

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

A plugin that imports an interface it was not granted is never instantiated; a call outside a
grant's scope is refused and recorded on the event stream.

Runs are **durable**. Each step of the loop is one atomic commit to a log, stored before
anyone sees it, and a run that was cut short is picked up from the log:

```rust
let events = Arc::new(corduroy_harness::jsonl::open("run.jsonl")?);
let harness = Harness::resume(provider, events)?;       // sessions come back from the file
harness.continue_run(&MAIN.into()).await?;              // finish what a crash interrupted
```

Each tool call is logged when it starts and when it returns, so after a crash a finished
call's result is reused, a call that never started simply runs, and the one that was running
is repeated only if its tool says that is safe; otherwise the model is told and decides.

**Gates** decide whether a tool call may run. Any plugin can be one, WebAssembly included;
the strictest answer wins, and an "ask" makes the run wait, across restarts, for a person:

```rust
harness.plugin(runtime.load("plugins/…/approval.wasm")?.config(r#"{"mode":"smart"}"#));
if let Status::Waiting { asks } = harness.run("tidy the repo").await?.status {
    harness.answer(&MAIN.into(), &asks[0].call, Answer::Allow).await?;   // run the write
}
```

## Try it

```bash
cargo plugins                                   # build the WebAssembly plugins
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
plugin and exactly what it is granted. `corduroy run <prompt>` runs one prompt; `corduroy
check` says what the agent has without calling a model; `corduroy log` prints the event
log, which is kept in `.corduroy/log.jsonl`. Kill it mid-run and `corduroy continue` picks
the run up; leave a question unanswered and the run waits, in the log, until you do.

## Where the ideas come from

| From | Idea | Where it lives |
| --- | --- | --- |
| [Cordis](https://github.com/cordiverse/cordis) (behind DeepSeek Harness) | Spatio-temporal composability: a plugin's effects revert when it unloads, and plugins follow the services they depend on | `corduroy-core::context` |
| [Prime Agent](https://github.com/PrimeIntellect-ai/prime-agent) | Long-lived kernels that keep state between tool calls | `corduroy-core::kernel` |
| [Hermes Agent](https://github.com/NousResearch/hermes-agent) | Skills the agent writes and repairs for itself | `plugins/skills` |
| [Pi](https://github.com/earendil-works/pi) | A minimal toolset: read, write, edit, bash | `plugins/pi-tools` |
| [OpenHands](https://github.com/OpenHands/software-agent-sdk) | An append-only event stream as the source of truth, and a loop you can program | `corduroy-core::event`, `src/session.rs` |
| [Goose](https://github.com/aaif-goose/goose) | The base abstractions, taken from its oi-kit crates | `src/harness.rs` |

[`docs/design.md`](docs/design.md) has the research behind each, what was borrowed, and what
is deliberately left for later.

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

Requires Rust 1.97+, the `wasm32-unknown-unknown` target (installed automatically from
`rust-toolchain.toml`) and [`cargo-nextest`](https://nexte.st)
(`cargo install cargo-nextest --locked`). Nothing needs a network or an LLM: the tests and the
demo drive the loop with a scripted provider.

## Layout

| Path | Purpose |
| --- | --- |
| `crates/corduroy-core` | Dependency-free core: plugin contexts, capabilities, event stream with sessions and a `Log` trait, the registries plugins contribute to (tools, prompts, kernels, gates, services, models, secrets, HTTP clients) |
| `crates/corduroy-cli` | The `corduroy` command: an agent file (providers, kernels, secrets, plugins and their grants) run in a terminal |
| `crates/corduroy-providers` | Model providers from `oi-kit-providers`: Anthropic, OpenAI, Ollama and the providers Goose knows |
| `crates/corduroy-http` | The HTTP client behind the `http` capability, which never follows a redirect |
| `crates/corduroy-shell` | A persistent POSIX shell as a kernel |
| `crates/corduroy-jsonl` | A JSON Lines file as the durable log behind an event stream |
| `crates/corduroy-wasm` | wasmtime host that loads components as capability-gated plugins |
| `src/` | `corduroy-harness`: the agent loop assembled from oi-kit, journaled to the event stream one commit per step; sessions, continuing an interrupted run, waiting for a person, and the model as a service |
| `wit/corduroy.wit` | The plugin world: host capabilities in, tools / event handlers / prompt text out |
| `sdk/js`, `sdk/python` | SDKs for plugins in JavaScript/TypeScript and Python, and the script host for automations written as scripts |
| `examples/` | The same `notes` plugin in JavaScript and Python |
| `plugins/guest` | `corduroy-guest`: write a plugin in Rust by implementing one trait |
| `plugins/pi-tools` | read, write, edit, and bash on a persistent kernel |
| `plugins/skills` | Agent-written `SKILL.md` files with a prompt index and a review nudge |
| `plugins/approval` | A gate: permission modes with allow and deny lists |
| `plugins/testbed` | For the host's own tests: makes host calls, loads plugins, echoes events and traps on demand |
| `crates/corduroy-test-support` | Test-only helpers: finds the built plugins, makes scratch directories |
| `tests/`, `crates/*/tests/` | Integration tests through the public API |

See [`CLAUDE.md`](CLAUDE.md) for contributor and agent guidelines.

## License

[Boost Software License 1.0](LICENSE)
