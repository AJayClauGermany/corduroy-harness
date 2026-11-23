# Develop corduroy

How to build, test and check a change to corduroy itself. The rules the code follows (lint
policy, architecture rules, style) are in [`CLAUDE.md`](../../CLAUDE.md); read it before a
larger change.

## Set up

You need [rustup](https://rustup.rs). The pinned toolchain, its components and targets
install themselves from `rust-toolchain.toml` on the first `cargo` command. Then:

```bash
cargo install cargo-nextest --locked
```

For the JavaScript and Python plugins and their tests, Node.js 20+ and Python 3.11+.

## Build

| Command | Builds |
| --- | --- |
| `cargo plugins` | the Rust add-ons in `plugins/` to WebAssembly. **Run it after changing `plugins/` or `wit/`**: the host's tests load the result |
| `scripts/guests.sh` | the JavaScript and Python plugins. Run it after changing `sdk/`, `examples/` or `wit/` |
| `cargo build` | the host workspace, the `corduroy` command included |
| `cargo corduroy <args>` | build and run the command from this tree |
| `cargo demo` | a scripted agent through two add-ons, printing the event stream |

## Test

| Command | Runs |
| --- | --- |
| `cargo t` | every host test (nextest). Fails with instructions if the add-ons are missing or stale |
| `cargo test --doc` | doctests, which nextest skips |
| `cargo test --manifest-path plugins/Cargo.toml` | the add-ons' own logic, natively |
| `cargo eval` | the agent doing real small tasks against a local model (`scripts/local-model.sh start` first); minutes, not run in CI |

Tests never need a network or a model: they drive the loop with `ScriptedProvider`. A test
that does not end is killed after 15 seconds.

## Check

```bash
cargo fmt --all
```

```bash
cargo lint
```

```bash
scripts/check.sh
```

`cargo lint` is clippy with every warning an error, under a strict lint set. Fix what it
finds rather than silencing it. `scripts/check.sh` runs exactly what CI runs: formatting,
clippy (including the core and loop built for a microcontroller without `std`), the add-ons,
every test, doctests and the API docs. A change is done when it passes.

## Where things go

- A new **tool, prompt, gate or reaction** is an add-on in `plugins/` (or a guest-language
  example), never code in the core or the loop.
- A new **kind of ability** that needs native code is an extension, in its own crate or in
  `corduroy-std`.
- A new **capability** is a new interface in `wit/corduroy.wit`, a host implementation in
  `crates/corduroy-wasm/src/plugin/host.rs` and an entry in `CAPABILITIES`; and an addition
  to [the capabilities reference](../reference/capabilities.md).
- The four stable contracts (the WIT world, the core's traits, the log, the agent file) change
  only by adding. See [What is stable](../design.md#what-is-stable).

## Documentation

These pages follow [Diátaxis](https://diataxis.fr). When you change behaviour, update the
page of the right kind: a key or a topic in **reference**, a recipe in **how-to**, the
reasoning in **explanation** (`docs/design.md` for design decisions). Keep the tutorials
working end to end; they are the first thing a new user runs.
