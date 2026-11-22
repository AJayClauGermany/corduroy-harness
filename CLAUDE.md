# corduroy-harness

A plugin-first agent harness in Rust. A small core provides a plugin tree, an event stream and
the registries plugins contribute to; everything else (the model, tools, prompt text,
kernels, reactions to events) is a plugin, either native Rust or a capability-gated WebAssembly component. The agent loop is
the harness's own (`src/harness.rs`), over the core's `Message`; oi-kit is used only behind
the provider boundary, in `corduroy-providers`. `docs/design.md` explains where each idea
comes from and why.

Three tiers: the **core** (the contract; changes rarely), **extensions** (native plugins:
trusted, compiled in, extending what the core can do) and **add-ons** (WebAssembly plugins:
granted capabilities, plugged in without a rebuild). **Plugin** covers both of the last two:
anything plugged in through `corduroy_core::Plugin` and unplugged again. The line between
them is how they get in, not what they contribute: an extension needs the command rebuilt,
an add-on is a `.wasm` file. Identifiers that predate these terms (`[[plugins]]`, the
`plugins` capability, `plugin/load`, `plugins/`, `WasmPlugin`) say *plugin* and reach
add-ons only; keep them. `docs/design.md` says what goes where ("Three tiers") and which
contracts must not break ("What is stable").

## Layout

| Path | What it is |
| --- | --- |
| `crates/corduroy-core` | **`no_std` + `alloc`, synchronous.** Zero dependencies with the default `std` feature; without it, its lock comes from the `critical-section` feature (the device supplies the implementation) and kernels are gone (see "Without the standard library" in `docs/design.md`). Plugin contexts (`App`, `Context`, `Plugin`), capabilities (`Grants`, `Scope`), the call scope (`call`: which session and tool call a thread serves, and waiting for a signal), the `EventStream` (commits, `SessionId`, the `Log` trait), and the `Tools` / `Prompts` / `Kernels` / `Gates` / `Services` / `Models` / `Secrets` / `HttpClients` registries. The registries hold traits only: kernels, stores, models and clients are implemented elsewhere. |
| `crates/corduroy-jsonl` | A store for the event stream: `JsonlLog`, one line per commit under a versioned header. Stores implement `corduroy_core::Log` and live outside the core. |
| `crates/corduroy-cli` | The `corduroy` binary (`cargo corduroy`), a thin `main` over the library: `agent` reads the agent file (TOML: providers, kernels, secrets, extensions, WebAssembly plugins with grants) and assembles the harness, protecting the log and the agent file; `extension` holds the extension kinds a command was built with; `command` is the command line (`main_with`); `terminal` shows events as they commit and answers gate asks. End-to-end tests drive the binary against a fake OpenAI-compatible server on loopback; `tests/local_model.rs` holds the evals (ignored by default), which drive it against a real small model. |
| `crates/corduroy-mcp` | `mcp_server`: one MCP server as a native plugin, its tools as `<server>__<tool>`. A stdio server is started through `Kernel::spawn`, never directly. Tests use a Python fixture server (`tests/fixtures/server.py`). |
| `crates/corduroy-providers` | Real model providers built from `oi-kit-providers` (`ProviderSpec` → `Arc<dyn corduroy_harness::Provider>`), converting messages, tools and usage at the boundary (`adapter`). The only crate that sees oi-kit's types. |
| `crates/corduroy-http` | `UreqClient`, the `HttpClient` behind the `http` capability (no redirects), and the `http_client` plugin. |
| `crates/corduroy-shell` | `ShellKernel`, a persistent POSIX shell as a `Kernel` that can also `spawn` processes, and the `shell_kernel` plugin. |
| `crates/corduroy-wasm` | wasmtime host. Loads components built against `wit/corduroy.wit` as `Plugin`s; links only granted interfaces. |
| `src/` (`corduroy-harness`) | Features `wasm`, `shell`, `jsonl` (default on; the command enables them, other members take none) re-export the add-on host, the shell kernel and the file log; without them the harness carries no WebAssembly runtime. The loop and its parts: `Harness` (sessions, `run_in`, `continue_run`, `answer`, `signal`, `stop`, `step`, and the loop itself), `Operation` (`src/operation.rs`), `Journal` (one step's effects to one commit), `message` (the `message` event's JSON, and the oi-kit shape old logs hold), `GateOperation`, `CompactOperation`, the `sessions` service for child sessions (`src/children.rs`), `PluginTools`, the `Providers` registry and `provider` plugin (`src/provider.rs`), `ScriptedProvider`. Async (tokio). |
| `wit/corduroy.wit` | The typed plugin boundary. Each imported interface is one capability. |
| `plugins/` | The add-ons. **Separate workspace**, built for `wasm32-unknown-unknown`: `guest` (the `corduroy-guest` crate every Rust plugin builds on: bindings generated once, a `Plugin` trait with default exports, `export!`), `pi-tools` (read, write, edit, bash, tree), `skills`, `approval` (a gate: permission modes), the Goose-baseline plugins `hints`, `todo`, `memory`, `recall`, `summarize`, `compact` (the `compaction` service), `subagents` (`delegate`), `inspect` (a gate) and `plugin-manager`, and `testbed` (for the host's tests only: host calls, loads, echoes and traps on demand). |
| `sdk/js`, `sdk/python` | Guest SDKs: write a plugin in JavaScript/TypeScript or Python; `corduroy-js build` / `corduroy-py build` make a component importing only the listed capabilities. `sdk/js/script-host` is the script host (automations as scripts). |
| `examples/` | The same `notes` plugin in each guest language, built by `scripts/guests.sh`; `local-model/corduroy.toml`, the default agent with its model served on this machine (the agent `cargo eval` runs). |
| `crates/corduroy-test-support` | Test-only helpers: finds built plugins, makes scratch directories. |

## Commands

| Command | What it does |
| --- | --- |
| `cargo plugins` | Build the add-ons (`plugins/`) to WebAssembly. **Run after changing `plugins/` or `wit/`**; host tests and the demo load the result. |
| `scripts/guests.sh` | Build the JavaScript and Python plugins (needs Node.js 20+ and Python 3.11+; installs the toolchains on first use). **Run after changing `sdk/`, `examples/` or `wit/`**; their tests skip when they are not built, unless `CORDUROY_REQUIRE_GUESTS` is set (CI). |
| `cargo t` | Fast tests for the whole host workspace (`cargo nextest run --all-features`). **Does not run doctests.** Fails with instructions if the plugins are missing or stale. |
| `cargo test --doc` | Doctests (nextest skips them) |
| `cargo test --manifest-path plugins/Cargo.toml` | Unit tests of the plugins' pure logic, run natively |
| `cargo lint` | Strictest clippy on the host workspace, `-D warnings`. Must be clean before finishing. |
| `cargo fmt --all` | Format. `cargo fmtc` checks without writing. |
| `cargo corduroy` | Run the `corduroy` command from this tree (`cargo corduroy check`, `cargo corduroy run <prompt>`; the alias already ends in `--`). Needs `cargo plugins` first. |
| `scripts/local-model.sh start` | Serve a small model (LFM3 1.7B, 4-bit) on `127.0.0.1:8080` as an OpenAI-compatible endpoint: builds llama.cpp's `llama-server` and fetches the model on first use (~4 min), both pinned by SHA-256. `stop` stops it. |
| `cargo eval` | The evals: the agent in `examples/local-model/` doing small real tasks against that model, checked by outcome. Minutes, not seconds; needs `cargo plugins` and the server. |
| `cargo demo` | Run a scripted agent through both plugins and print the event stream |
| `scripts/check.sh` | fmt + clippy + plugins + nextest + doctests + docs, host and plugins: exactly what CI runs |

If `cargo nextest` is missing: `cargo install cargo-nextest --locked` (slow, ~3 min).

## Definition of done

`scripts/check.sh` passes. Fix lint findings; do not silence them.

## Automation (Claude Code hooks)

Configured in `.claude/settings.json`, scripts in `.claude/hooks/`.

- **After every Edit/Write/MultiEdit** of a `*.rs`, `*.wit`, `Cargo.toml` or `clippy.toml` file,
  `lint-after-edit.sh` runs clippy on the workspace the file belongs to (host, plugins, or both
  for the WIT world) and returns the findings as feedback on that edit. Treat them as
  instructions to fix, not noise. A failure partway through a multi-file change is expected;
  finish the change, then fix what remains.
- **On cloud session start**, `session-start.sh` makes sure clippy, rustfmt, the wasm32 target
  and cargo-nextest are installed, then builds the plugins. The first start in a fresh
  container builds nextest from crates.io (~4 min, the prebuilt binaries at `get.nexte.st` are
  blocked by the egress policy); later starts skip it.

## Lint policy

Clippy runs with `all`, `pedantic`, `nursery`, `cargo` plus a curated set of `restriction`
lints (see `[workspace.lints]` in `Cargo.toml`, thresholds in `clippy.toml`). Treat its output
as design guidance, not noise.

- Never add a crate- or module-wide `#![allow(...)]`, and never loosen `[workspace.lints]` to
  get green.
- If one exception is genuinely right, use `#[expect(lint, reason = "...")]` on the narrowest
  item. `allow_attributes` is on, so plain `#[allow]` is flagged; `expect` self-removes when stale.
- The one standing exception: crates that depend on wasmtime, oi-kit or rmcp (whose version
  oi-kit fixes), directly or through another of ours, carry
  `#![expect(clippy::multiple_crate_versions)]`, because the duplicates
  are in those dependencies' own trees. `corduroy-core` has no such exception and must stay dependency-free with `std` (its one
  dependency, `critical-section`, is optional and only for builds without it).
- `unwrap`, `expect`, `panic!`, `todo!`, indexing and slicing are denied in non-test code. Return
  errors instead. They are permitted inside `#[cfg(test)]` and `#[test]` functions, but **not in
  helper functions of integration tests**: make those return `Result` and unwrap in the test.
- No `println!`/`eprintln!`/`dbg!`. Write to an `io::Write` you were given (see `src/main.rs`).
- `unsafe` is forbidden in the host workspace.
- Public items need docs (`missing_docs`), fallible public fns need `# Errors`.
- `redundant_pub_crate` and `unreachable_pub` pull in opposite directions for private modules.
  Keep crate-internal items either in a public module as `pub(crate)`, or in a private child
  module as `pub(super)` (see `crates/corduroy-wasm/src/plugin/`).
- `plugins/` has its own, lighter lint table: the bindings `wit-bindgen` generates need
  `unsafe` and are confined to `corduroy-guest`'s `mod bindings` with a documented `#[allow]`.
  A Rust plugin implements `corduroy_guest::Plugin` and calls `corduroy_guest::export!`; it
  does not generate bindings of its own. Plugin code must not panic: a panic is a trap that
  takes the instance down.

## Architecture rules

- **The core builds without `std`.** `corduroy-core` is `#![no_std]`: use `core::`/`alloc::`
  paths, `crate::sync::Lock` for shared state (never a `std::sync` lock outside
  `#[cfg(feature = "std")]`), and no 64-bit atomics (32-bit microcontrollers lack them).
  Anything that needs an operating system (a thread, the clock, a file, a process) goes
  behind the `std` feature, as an addition. `scripts/check.sh` lints the core for
  `thumbv7em-none-eabihf` without `std`, and the harness without its default features.
- **Everything is a plugin.** The core and the harness provide registries and a loop, never a
  tool or a prompt of their own. New behaviour goes in a plugin unless it is a new *kind* of
  thing plugins contribute.
- **The stable contracts change by adding.** The WIT world, the core's traits, the log and
  the agent file (see "What is stable" in `docs/design.md`). A new host ability is a new
  import interface, never a changed export; a new trait method has a default; a struct
  that may grow is `#[non_exhaustive]` with a constructor; a log line gains fields and a
  new schema version, and old versions stay readable.
- **An extension is a native plugin, not a second mechanism.** It contributes through the
  same `Context`: core-trait implementations, services, decision points (`Gates::declare`)
  and loop steps (the harness's `Operations` registry). It never adds a host function to
  the WebAssembly linker; what a plugin needs from it goes through a `Service`.
- **A name belongs to one plugin.** Events, the `state` store and service calls identify a
  plugin by name, so `App` does not apply a second plugin of a name already held, nor one
  named `host`, `agent`, `user` or `environment`. Give native plugins names that cannot
  collide with a plugin's (`kernel-<name>`).
- **Decision points are declared.** Ask the gates only at a point declared in `Gates`
  (the core's two, or one an extension declared); an undeclared point is denied.
- **Every registration is an effect.** Plugins set things up only through their `Context`
  (`ctx.effect`, `ctx.provide`, `ctx.on`, `registry.register(ctx, ..)`), so unloading reverts
  it. Never reach around the context to mutate shared state from `apply`.
- **Plugins declare what they need.** A service used in `apply` must be listed in `inject`; the
  plugin is then activated, suspended and re-applied as that service comes and goes.
- **No ambient authority for WebAssembly plugins.** A host function is reachable only through
  a WIT interface, each interface is one capability, and every host function checks its
  resource against the grant's scope before acting. Do not add WASI to the linker. A new host
  ability means a new interface in `wit/corduroy.wit`, a `Host` impl in
  `crates/corduroy-wasm/src/plugin/host.rs` and an entry in `CAPABILITIES`.
- **A trap kills an instance, not the plugin.** The next call re-instantiates it from its
  `Blueprint` and activates it again, up to `Limits::restarts`; then it is stopped until
  reloaded. A plugin must not rely on memory surviving a trap: durable state goes through the
  host.
- **Never call back into a plugin while it is running.** Events a plugin raises are queued and
  delivered after its call returns (`Instance::call`). A call that would re-enter a plugin
  on the same call chain is refused, and a nested call waits at most `Limits::nested_wait`
  for a plugin busy elsewhere; never add a wait without a bound inside a plugin's call.
- **A gate does not make tool calls.** A WebAssembly plugin that gates `tool/call` and
  imports `tools` is refused at activation; keep the check if either side changes.
- **Plugins load plugins only through `Requests`.** A host call cannot change the plugin tree;
  it queues the change (`App::requests`) and the tree applies it when it next settles (the
  harness settles it before every loop step). A child's grants are always
  `Scope::within` its loader's, and every load is asked of the gates at `plugin/load`.
- **Protected paths stay out of reach.** `WasmRuntime::protect` paths (the host binary and
  the disk cache from the start) are refused to `fs-read` and `fs-write` before any scope
  check; any new path-taking host function must go through `HostState::path`.
- **The model is a service.** The loop asks the provider registered under its name in
  `Providers` on every request (`provider::Routed`); never hold a provider elsewhere.
  `Harness::new` registers its argument there. Plugins reach models through the core's
  `Models` registry (the `llm` capability), never through the loop's provider.
- **Secrets never reach a plugin or the log.** `secret` hands out a `credential` handle;
  host functions that take one write the value into what they send and record only the
  secret's name. Do not record response bodies, which may echo it. An `HttpClient` must not
  follow redirects: the host checked one host against the grant.
- **Host calls that wait on the outside world are bounded.** `llm` and `http` pass
  `Limits::outbound_wait` to the model or client and stop waiting shortly after it
  themselves (`host::bounded`), whether or not the implementation honours it.
- **Plugin state lives on the stream.** `state` writes are `state/set` / `state/remove`
  events from the plugin, kept only if the call returns; the store is rebuilt from them, so
  `events.emit` refuses `state/*` topics.
- **The event stream is the record.** The conversation is a projection of `message` events
  (`src/session.rs`), and of `compaction` events, which hide what they summarise from the
  model without removing it; do not keep conversation state that cannot be rebuilt from the
  stream.
  The same goes for sessions (`session/start`) and for whether a run is unfinished.
- **Commit, then deliver.** A step's events go to the stream as one `EventStream::commit`,
  which stores the batch in the `Log` before any listener or the next step sees it. Events of
  a run carry its `SessionId`. A refused commit closes the stream and is the one event failure
  that must reach the caller: propagate the `LogError` where there is a caller to give it to.
- **Each tool call has its own trail.** `PluginTools` commits `tool/start` before a tool runs
  and `tool/result` when it returns; `continue_run` reads them to tell a call that never
  started, one that was running and one that finished. Anything else that runs tools must
  do the same through the session's `Journal`.
- **A tool is repeated after a crash only if it says so.** `ToolSpec::replay_safe` (and
  `replay-safe` in the WIT) is true only when calling twice equals calling once.
- **Decisions go through gates, observations through events.** A step that needs an answer
  before something happens asks the `Gates` registry at a decision point in
  `corduroy_core::POINTS`; the strictest verdict wins and is recorded before the thing
  happens. A gate that cannot answer denies. Do not give events return values.
- **Waiting is in the log.** A run waiting for a person (a gate asked) or for a signal (a
  tool call that waits, such as one that started a child session) has no `turn/end`;
  `Harness::answer` and `Harness::signal` resume it and `continue_run` must not count it as a
  restart. A tool waits only through `corduroy_core::call::wait_for`, inside the call scope
  `PluginTools` sets; never block a tool call on another run.
- **Every world export is mandatory for every guest.** Adding one means a stub in each
  plugin under `plugins/`, and a check that JavaScript and Python can implement it.
- **A tool returns content blocks.** `Tool::call` gives a `ToolOutput` (text and images);
  text converts into one (`Ok(text.into())`, or return a `String` from `tool_fn`). Build a
  `ToolSpec` with `ToolSpec::new(..)` and its builder methods, never as a literal. Whatever
  records a `tool/result` writes the text as `output` and the blocks as `content` when the
  result is not exactly one text block (`src/session.rs` and the host's `tools.call`).
- **Tool failures are data, not control flow.** A `ToolError` becomes an error tool result and
  goes back to the model so it can recover. A model failure ends the run as `Status::Failed`,
  also recorded in the conversation. Only a broken pipeline returns `Err` from `Harness::run`.
- Hitting the turn limit is a normal `Status::TurnLimitReached`, not an `Err`.
- **The conversation is the core's `Message`.** Steps, providers, the log and plugins all
  speak it; a provider converts to and from its API's shape itself (`corduroy-providers`'
  `adapter`). Never let oi-kit's (or any API's) types into the harness or the log.
- **A run can be stopped.** `Harness::stop` sets the run's `Stop`, which tools see through
  `corduroy_core::call::stopping`. Anything that can run long on a run's behalf (a kernel
  cell, a tool loop) looks now and then and returns early; never block a run in a way a
  stop cannot end.
- Keep the loop ignorant of any specific model, tool or plugin. Extend it with an
  `Operation` (`Harness::with_operation` for the host, the `Operations` registry for a
  plugin), not with special cases.
- Tests must not need network or an LLM. Drive the loop with `ScriptedProvider`. The one
  exception is the evals (`cargo eval`), which are `#[ignore]`d so `cargo t` and CI never run
  them, and which reach a model only on loopback through ordinary configuration. An eval checks
  outcomes (files, exit codes, tool results, a fact in the answer), never phrasing; a local model needs no
  change to the core or the harness, so never add one to make an eval pass.
- Unit tests live in a `#[cfg(test)] mod tests` beside the code; behaviour through the public API
  goes in `tests/`. nextest kills tests after 15s (`.config/nextest.toml`), so a non-terminating
  loop shows up as a failure rather than a hang.

## Style

- `rustfmt.toml` sets width 100. Run `cargo fmt --all` before committing.
- Prefer `.to_owned()`/`String::from` over `.to_string()` on `&str` (`str_to_string` is on).
- Use `foo.rs` + `foo/` for modules, never `foo/mod.rs` (`mod_module_files` is on).
- Inline format args: `format!("{name}")`, not `format!("{}", name)`.
- oi-kit is pinned to an exact alpha version, and `rmcp` must match the version oi-kit builds
  against. Bump them together.
