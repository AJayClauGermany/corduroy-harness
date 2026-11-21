# corduroy-harness

A plugin-first agent harness in Rust. A small core provides a plugin tree, an event stream and
a programmable loop; **everything else is a plugin**: the tools a model can call, the text in
its system prompt, the kernels it computes in, and the reactions to what happens.

Plugins are native Rust or **WebAssembly components** typed with [WIT](wit/corduroy.wit), so
they can be written in any language that compiles to a component. A WebAssembly plugin starts
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
| `crates/corduroy-core` | Dependency-free core: plugin contexts, capabilities, event stream, tool / prompt / kernel registries |
| `crates/corduroy-wasm` | wasmtime host that loads components as capability-gated plugins |
| `src/` | `corduroy-harness`: the agent loop assembled from oi-kit, journaled to the event stream |
| `wit/corduroy.wit` | The plugin world: host capabilities in, tools / event handlers / prompt text out |
| `plugins/pi-tools` | read, write, edit, and bash on a persistent kernel |
| `plugins/skills` | Agent-written `SKILL.md` files with a prompt index and a review nudge |
| `tests/`, `crates/*/tests/` | Integration tests through the public API |

See [`CLAUDE.md`](CLAUDE.md) for contributor and agent guidelines.

## License

[Boost Software License 1.0](LICENSE)
