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
| `crates/corduroy-jsonl` | A store for the event stream: `JsonlLog`, one line per commit under a versioned header, and `remove`, which takes whole sessions out between runs. Stores implement `corduroy_core::Log` and live outside the core. |
| `crates/corduroy-cli` | The `corduroy` binary (`cargo corduroy`), a thin `main` over the library: `agent` reads the agent file (TOML: providers, kernels, secrets, extensions, WebAssembly plugins with grants) and assembles the harness from a `corduroy_std::Standard` (`assemble_from`), protecting the log and the agent file; `extension` re-exports the extension kinds; `command` is the command line (`main_standard`, `main_with`, and `run_on`, the same with its arguments, input and output handed over), with `secrets`, `init` and `doctor` in modules under it; `terminal` shows events as they commit and answers gate asks, racing what is typed (`Lines`, read on a thread of its own) against an answer recorded at a front end. `report` reads what a run did and how long each part took off the log (`corduroy report`). End-to-end tests drive the binary against a fake OpenAI-compatible server on loopback; `tests/keychain.rs` runs the command in the test's own process (`run_on`) with a keychain in memory, never the system's; `tests/local_model.rs` holds the evals (ignored by default), which drive it against a real small model. |
| `crates/corduroy-mcp` | `mcp_server`: one MCP server as a native plugin, its tools as `<server>__<tool>`. A stdio server is started through `Kernel::spawn`, never directly. Tests use a Python fixture server (`tests/fixtures/server.py`). |
| `crates/corduroy-providers` | Real model providers built from `oi-kit-providers` (`ProviderSpec` → `Arc<dyn corduroy_harness::Provider>`), converting messages, tools and usage at the boundary (`adapter`). The only crate that sees oi-kit's types. |
| `crates/corduroy-http` | `UreqClient`, the `HttpClient` behind the `http` capability (no redirects), and the `http_client` plugin. |
| `crates/corduroy-chatgpt` | Sign in with ChatGPT (OpenAI's open-source token-sharing flow): `ChatGpt::sign_in` (dynamic registration, PKCE, loopback callback, ID token verified against the issuer's keys), `Store` (the sign-in, in a `Vault` such as the keychain, or in the person's own directory, mode 0600), `ChatGpt::token` (renewal under a file lock), `chatgpt_secret`, the token as a `Secret`, and `advice` (what to tell the person when the plan refuses). Tests run against a fake issuer on loopback. Tokens never go in an error, a log or `Debug`. |
| `crates/corduroy-keychain` | The operating system's keychain: `Keychain` (reachable, get, set, remove one item under the service `corduroy`), `Os` (the macOS Keychain, the Windows Credential Manager, the Secret Service over D-Bus; a value longer than one Windows credential is kept as numbered pieces), `Memory` and `Missing` (for tests: one that forgets, one that is not there), and `keychain_secret`, an item as a `Secret`, read once for a run. No fallback where a machine has none. Its one test of the real keychain runs only where `CORDUROY_REQUIRE_KEYCHAIN` is set (CI, on all three systems), so a test run never touches a person's own. A value never goes in an error, a log or `Debug`. |
| `crates/corduroy-page` | `page`: the agent's page in a browser, served on `127.0.0.1` while the host drives runs, as a native plugin (`[extensions.<name>] kind = "page"`). It serves the page in `plugins/viewer/page/` and the events as the scripts that page loads, and takes back one thing, an answer to a gate's question, which it records through `Outside`. It registers a `Front`; its address holds a token, new at each opening and never recorded. Tests drive it over a loopback socket. |
| `crates/corduroy-shell` | `ShellKernel`, a persistent shell (POSIX, or PowerShell: `ShellKernel::powershell`) as a `Kernel` that can also `spawn` processes, and the `shell_kernel` plugin. Its PowerShell tests skip without PowerShell unless `CORDUROY_REQUIRE_POWERSHELL` is set (CI, on Linux and Windows). |
| `crates/corduroy-wasm` | wasmtime host. Loads components built against `wit/corduroy.wit` as `Plugin`s; links only granted interfaces. |
| `src/` (`corduroy-harness`) | **`no_std` + `alloc`, any executor**, like the core: features `std`, `tokio` (default; tool calls on tokio's blocking pool), `critical-section`; `wasm` (default on, implies `std`) re-exports the add-on host. It carries no extensions: the shell kernel, the file log, providers and the rest are `corduroy-std`'s. The loop and its parts: the `Loop` trait, the `Loops` registry and `StandardLoop` (`src/loops.rs`; what each step does, swappable by name), `Harness` (sessions, `run_in`, `continue_run`, `answer`, `signal`, `stop`, `step`, and the loop itself), `Operation` (`src/operation.rs`), `Outside` and the `Fronts` registry (`src/outside.rs`: what an extension may do to runs from outside them, and what the host opens before it drives), `Journal` (one step's effects to one commit), `message` (the `message` event's JSON, and the oi-kit shape old logs hold), `GateOperation`, `CompactOperation`, the `sessions` service for child sessions (`src/children.rs`), `PluginTools`, the `Providers` registry and `provider` plugin (`src/provider.rs`), `ScriptedProvider`. Async (tokio). |
| `wit/corduroy.wit` | The typed plugin boundary. Each imported interface is one capability. |
| `plugins/` | The add-ons. **Separate workspace**, built for `wasm32-unknown-unknown`: `guest` (the `corduroy-guest` crate every Rust plugin builds on: bindings generated once, a `Plugin` trait with default exports, `export!`), `pi-tools` (read, write, edit, bash, tree), `skills`, `approval` (a gate: permission modes), the Goose-baseline plugins `hints`, `todo`, `memory`, `recall`, `summarize`, `compact` (the `compaction` service), `subagents` (`delegate`), `inspect` (a gate) and `plugin-manager`; `review` (a second model reviews the change); `search` (`grep` and `find` through `fs-read` alone), `fetch` (`web_fetch`), `today` (the date in the prompt) and `viewer` (the log as a page of files, in `plugins/viewer/page/`); and `testbed` (for the host's tests only: host calls, loads, echoes and traps on demand). |
| `sdk/js`, `sdk/python` | Guest SDKs: write a plugin in JavaScript/TypeScript or Python; `corduroy-js build` / `corduroy-py build` make a component importing only the listed capabilities. `sdk/js/script-host` is the script host (automations as scripts). |
| `docs/` | User documentation, organised by [Diátaxis](https://diataxis.fr) from `docs/README.md`: `tutorials/`, `how-to/`, `reference/` (command, agent file, capabilities, shipped add-ons, events), `explanation/`; plus `design.md` (the design record), `guest-languages.md` and `open-questions.md` (the decisions still to make before 1.0: questions and directions, never answers; a decided entry is deleted there and its answer written where it belongs). **Update the matching reference page when you change a command, an agent-file key, a capability, a shipped add-on's tools or configuration, or an event topic.** |
| `examples/` | `demo.rs` (`cargo demo`: a scripted model through the shell kernel and two add-ons); the same `notes` plugin in each guest language, built by `scripts/guests.sh`; `local-model/corduroy.toml`, the default agent with its model served on this machine (the agent `cargo eval` runs); `chatgpt-plan/corduroy.toml`, the same agent paid by a ChatGPT plan. |
| `crates/corduroy-std` | The standard extensions for an ordinary computer (macOS, Windows, Linux): `Standard`, the set as swappable parts (extension kinds: kernels as `<kind>-kernel`, `[mcp.*]` as `mcp`, `[secrets.*]` as `env-secret` or `<kind>-secret`, `http-client`; sign-ins by kind, which `corduroy login` runs; the keychain, with how a value is put into it (`Keep`, which `corduroy secrets set` runs) and how each kind of secret says it stands; extensions every agent gets; the log store; add-ons on or off), `Extensions`/`Asked`/`Places` (the CLI re-exports them), and re-exports of the std extension crates. |
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
- The one standing exception: crates that depend on wasmtime, oi-kit, rmcp (whose version
  oi-kit fixes) or the Secret Service client behind the keychain, directly or through
  another of ours, carry `#![expect(clippy::multiple_crate_versions)]`, because the
  duplicates are in those dependencies' own trees. `corduroy-core` has no such exception and must stay dependency-free with `std` (its one
  dependency, `critical-section`, is optional and only for builds without it).
- `unwrap`, `expect`, `panic!`, `todo!`, indexing and slicing are denied in non-test code. Return
  errors instead. They are permitted inside `#[cfg(test)]` and `#[test]` functions, but **not in
  helper functions of integration tests**: make those return `Result` and unwrap in the test.
- No `println!`/`eprintln!`/`dbg!`. Write to an `io::Write` you were given (see `examples/demo.rs`).
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
- **The loop builds without `std` and on any executor.** `corduroy-harness` follows the same
  rules as the core: no tokio outside `#[cfg(feature = "tokio")]`, no blocking on a runtime;
  wait through wakers (see `src/stop.rs`), and run blocking work through `tools::blocking`.
  `scripts/check.sh` lints it for `thumbv7em-none-eabihf`, and `tests/executor.rs` drives it
  with no runtime.
- **The loop is swapped, not special-cased.** What a step does is a `Loop` in the `Loops`
  registry; what every run must keep (plugin requests applied, stop, waits, children, the
  recorded end) stays in the harness, outside any loop.
- **What ships is a `Standard`, not wiring.** The CLI takes its parts from
  `corduroy_std::Standard`; a new part a platform may want to swap goes there, not as a
  hard-wired call in `agent.rs`.
- **Everything is a plugin.** The core and the harness provide registries and a loop, never a
  tool or a prompt of their own. New behaviour goes in a plugin unless it is a new *kind* of
  thing plugins contribute.
- **The stable contracts change by adding.** The WIT world, the core's traits, the log and
  the agent file (see "What is stable" in `docs/design.md`). A new host ability is a new
  import interface, never a changed export, or a function on an existing interface only
  where its grant already covers what the function does (see "How the plugin world grows"
  there); a new trait method has a default; a struct
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
- **One model call.** The loop and a plugin fill in the same `corduroy_core::ModelRequest`
  and read the same `ModelReply`; what a request or a reply gains is added there once. A
  model a plugin asks is named `<provider>/<model>` and offered no tools, and an add-on's
  `llm` grant is the only list of the models it may ask: never add a second one.
- **A kernel starts from an empty environment.** A shell, and any process a kernel starts,
  gets the plain variables (`corduroy_shell::PLAIN`) and what its entry sets or passes by
  name, never the command's own environment, which is where keys are. A new kernel does
  the same: clear the environment first, then add what was listed.
- **A front end is an extension.** The core, the loop and the command speak no wire
  protocol. Something that serves a page or an editor is an extension with a format of its
  own, driving through the handle every extension has (see "How people meet it" in
  `docs/design.md`). It registers a `Front` in `Fronts` and starts nothing until the host
  opens it, which the command does only when it is about to drive runs.
- **A front end records; the host drives.** What a person does at a front end goes through
  `Outside`, which records and returns; the host then takes the run on
  (`Harness::answered`). Never run a loop, or call `Harness::answer`, from a front end's
  own thread. `Outside` and `Fronts` are provided to native plugins by name and are not in
  the `Services` registry: do not put there anything that answers a gate's question, which
  is the person's to answer.
- **What lets a person in is never recorded.** A front end's address, token or anything
  else that admits someone is shown to the person (`Front::open`'s line) and kept out of
  the log, a `plugin/log` line included: the agent's own tools can read the log.
- **A credential's source is written in the agent file.** New code takes a key through
  `Secrets` and never looks one up in the environment by itself; `api_key_env` and
  `bearer_token_env` are to give way to naming a secret. Never fall back from one source
  to another without the file saying so: a keychain that is not there is an error, never a
  file or a variable in its place. The agent used when there is no agent file reads no key
  and no variable (it asks a local model); only `init`, which the person runs, looks at the
  environment to choose.
- **The safer source is the one that needs nothing written.** A `[secrets.*]` entry that
  says nothing is kept in the keychain, and a sign-in is kept there; the environment
  (`env`) and a file (`keep = "file"`) are what an entry has to say. Give a new kind of
  secret, or a new setting of one, the same shape: the default is the safe one.
- **The keychain is a part of the `Standard`.** Code reaches it through
  `Standard::keychain` (or the `Keychain` it was handed), never by making an `Os` of its
  own, so that a command can swap it and no test touches a person's real keychain: tests
  hand in `Memory` or `Missing`.
- **A trial is offered both ways and written down.** Where an open question is easier to
  answer by use, the code may offer both answers; each such pair is listed in
  `docs/trials.md` with the commands for each way. When it is answered, the way that lost
  is removed and the entry deleted.
- **The agent file's keys and kinds are ours.** Do not add one because a dependency accepts
  it. oi-kit's names for providers beyond the five promised kinds are shorthand, outside
  the promise, and one whose definition reads the environment is not to be offered.
- **What an add-on wants is a request.** A manifest grants nothing, a person's yes is kept
  for the grants and not the code, and only the person's own answer gives an add-on more
  than its loader holds.
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
  started, one that was running and one that finished. The trail is the harness's to write:
  a loop runs calls through `Run`, and nothing outside the harness records half of one by
  hand (see "What an extension can reach" in `docs/design.md`).
- **An operation reads and returns; a loop acts.** An `Operation` looks at the session and
  says what to record. A step that calls a tool or asks the model is a `Loop`, built from
  `Run`'s parts; a tool that needs an agent's work starts a session.
- **The session is the unit of concurrency.** Inside a session one thing happens at a time:
  one run, one step, one tool call. Sessions may run side by side, so nothing may depend on
  two of them overlapping, or on their not doing so; do not add state that is shared
  between sessions without a lock, or a wait on another session's run.
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
  plugin under `plugins/`, and a check that JavaScript and Python can implement it. The
  exports are meant to close at six: what the host newly asks of an add-on goes through
  `handle-call`, under a service name the harness defines, not through a seventh export.
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

- A command's name is as far in meaning from every other command's as it can be, so that no
  two are taken for each other: `check` says what an agent has, `doctor` whether it works,
  `secrets set` puts a key in, `login` signs in to a subscription. Do not add an alias for
  one because another word might be reached for; that is decided when someone does.
- `rustfmt.toml` sets width 100. Run `cargo fmt --all` before committing.
- Prefer `.to_owned()`/`String::from` over `.to_string()` on `&str` (`str_to_string` is on).
- Use `foo.rs` + `foo/` for modules, never `foo/mod.rs` (`mod_module_files` is on).
- Inline format args: `format!("{name}")`, not `format!("{}", name)`.
- oi-kit is pinned to an exact alpha version, and `rmcp` must match the version oi-kit builds
  against. Bump them together.
