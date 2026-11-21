# Design

corduroy-harness is a prototype of one idea: an agent harness in which nearly everything is a
plugin, plugins can be WebAssembly, and a WebAssembly plugin can do only what it was granted.
It takes one mechanism from each of six existing harnesses. This document records what each
of those mechanisms is (checked against the projects' sources in November 2026), what was
borrowed, where it lives here, and what was left out.

## The shape

```
                 ┌───────────────────────── corduroy-harness (src/) ─────────────────────────┐
                 │  Harness: oi-kit StateMachine over [custom ops, prompt, tools, limit, LLM]  │
                 │  Journal: effects ──► events            PluginTools: registry ──► oi-kit   │
                 └───────────────┬───────────────────────────────────────┬───────────────────┘
                                 │                                       │
        ┌──────── corduroy-core (no dependencies) ────────┐   ┌──── corduroy-wasm ────┐
        │ App / Context / Plugin   (fibers, effects)      │   │ WasmRuntime, WasmPlugin │
        │ Grants / Scope           (capabilities)         │◄──┤ linker = granted WIT    │
        │ EventStream              (log + pub/sub)        │   │ interfaces only         │
        │ Tools, Prompts, Kernels  (registries)           │   └───────────┬─────────────┘
        └─────────────────────────────────────────────────┘               │ wit/corduroy.wit
                                                              ┌───────────┴─────────────┐
                                                              │ plugins/pi-tools, skills │
                                                              │ (wasm32, any language)   │
                                                              └──────────────────────────┘
```

The core knows nothing about WebAssembly or models. The WebAssembly host knows nothing about
models. Only the top crate depends on oi-kit.

## The six inspirations

### Cordis: spatio-temporal composability

**What it is.** [Cordis](https://github.com/cordiverse/cordis) is the TypeScript plugin
framework from the Koishi ecosystem ("a meta-framework of spatiotemporal composability").
[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) (`dsh`) vendors it and
builds every subsystem as a plugin providing a service (`ctx.tools`, `ctx.llm`, `ctx.agentLoop`).

- *Temporal* composability: every registration (listener, service, child plugin, tool) is an
  effect whose disposer is recorded on the owning plugin instance (a fiber). Unloading runs
  the disposers in reverse, so a plugin's side effects are completely reverted.
- *Spatial* composability: a plugin declares `inject: ['tools']` and stays pending until that
  service exists. If the provider goes away the dependent is unloaded first, and it is applied
  again when the provider returns.

**Here.** `corduroy_core::context`. `App` is the fiber tree; `Context` is the only way a plugin
touches shared state. `ctx.effect`, `ctx.provide`, `ctx.on`, `ctx.plugin` and
`Registry::register(ctx, ..)` all record effects; `App::dispose` and `App::reload` unwind them
newest first, dependents before providers. A plugin whose `apply` fails is rolled back the same
way. `Context::service` refuses a service the plugin did not list in `inject`, as Cordis does.

**Left out.** `ctx.isolate` (per-subtree service instances), config-driven plugin trees
(`cordis.yml`), the five event dispatch modes (only plain emit exists), and async `apply`.

### Prime Intellect: long-lived kernels

**What it is.** [Prime Agent](https://github.com/PrimeIntellect-ai/prime-agent) gives the model
essentially one tool, `python_repl`. The kernel is a subprocess speaking newline-delimited JSON;
one namespace persists across cells, with timeouts that interrupt and then kill, an output cap,
snapshot and restore, and a bridge that lets code in the kernel call back into the host.

**Here.** `corduroy_core::kernel`. `Kernel` is the trait (`exec(code, timeout) -> Cell`),
`Kernels` the registry, and `ShellKernel` the reference implementation: a persistent POSIX
shell whose variables and working directory survive between cells. A cell that times out or
calls `exit` costs the kernel its state, and the error says so, as Prime's reset notice does.
WebAssembly plugins reach kernels through the `kernel` capability, scoped by kernel name; the
`bash` tool in `pi-tools` is a thin layer over it. A WebAssembly plugin is also a small kernel
in its own right: its instance memory lives as long as the plugin stays loaded.

**Left out.** A Python kernel, snapshot and restore, the kernel-to-host bridge (programmatic
tool calling from inside a cell) and sub-agents. Kernels are not sandboxes: a shell kernel can
do whatever the host user can, so *which kernel a plugin may use* is the security decision.

### Hermes: skills the agent writes for itself

**What it is.** [Hermes Agent](https://github.com/NousResearch/hermes-agent) stores skills as
`<category>/<name>/SKILL.md` with front matter. Only an index goes in the system prompt; the
model loads a body with `skill_view`. It writes skills with `skill_manage` (create, patch,
write_file, delete). After roughly ten tool-calling iterations without a skill being touched,
a background review forks the agent on the conversation with a prompt that prefers patching an
existing skill over creating a new one. A curator ages unused skills out, and agent-written
skills can be scanned and staged for approval.

**Here.** `plugins/skills`, a WebAssembly plugin holding `fs-read` and `fs-write` on its skills
directory and `events` on `skills/*`, nothing else. It contributes `skills_list`, `skill_view`
and `skill_manage` (create, patch by unique string or whole rewrite, delete), validates names
and front matter, and renders the `<available_skills>` index as a prompt part. It listens for
`tool/result` and `turn/end`; when enough tool calls pass without a skill being saved it emits
`skills/review-due` and adds a reminder to the prompt until one is.

**Left out.** The background review fork (the nudge is in-band here; `skills/review-due` is the
hook a reviewer would attach to), supporting files, usage tracking and the curator, threat
scanning and write approval, and the separate memory files.

### Pi: a minimal toolset

**What it is.** [Pi](https://github.com/earendil-works/pi) ships four default tools: `read`
(`path`, `offset`, `limit`), `write`, `edit` (a list of `oldText`/`newText` pairs, each unique,
all matched against the original) and `bash`. Output is capped at 2000 lines or 50 KB. Tools
carry their own prompt snippets, and everything else is an extension hooking typed events.

**Here.** `plugins/pi-tools`, with the same four tools, schemas and limits. It is one plugin
among others rather than a built-in: the harness has no tools of its own.

**Left out.** Fuzzy matching in `edit`, saving truncated `bash` output to a file, per-tool
prompt snippets, and the session tree. Pi's gate-style hooks have an equivalent in
`Harness::with_operation` (see `tests/agent_loop.rs`), but only for native code so far.

### OpenHands: event stream and programmable loop

**What it is.** OpenHands V0 was a threaded pub/sub `EventStream` of actions and observations
with `source` and `cause`. The V1 [SDK](https://github.com/OpenHands/software-agent-sdk) made
the events immutable and typed, the agent stateless, and the conversation a *view* over an
append-only log, with condensation as an event rather than a rewrite.

**Here.** `corduroy_core::event` is an append-only log with synchronous, ordered delivery, and
every event has a source and an optional cause. `src/session.rs` makes the conversation a
projection of it: the `Journal` turns each effect of the loop into `message`, `tool/call`
(caused by its message), `tool/result` (caused by its call) and `usage` events, and
`Harness::resume` rebuilds a session from a replayed stream. Plugin lifecycle, capability
denials and plugin-defined topics share the same stream, so plugins and the model see one
history.

**Left out.** Condensation, a security analyser and confirmation policy, and persistence to
disk (the log is in memory; `EventStream::replay` is the seam for a store).

### Goose: the base abstractions, from oi-kit

**What it is.** oi-kit ("Open Intelligence Kit") is the set of crates Goose is built from,
published as `oi-kit-*` from the [Goose repository](https://github.com/aaif-goose/goose);
there is no crate named just `oi-kit`. `oi-kit-provider-types` has `Provider`, `Message` and
`Conversation`. `oi-kit-agent` unrolls the agent loop into a state machine: an ordered list of
`Operation`s, each of which looks at the persisted conversation and either declines or returns
effects for the host to persist; inference is the last step.

**Here.** `src/harness.rs` uses those types directly. `Harness::steps` is the pipeline: custom
operations, the prompt, plugin tools, an answer for calls nobody handled, the turn limit, and
oi-kit's `InferenceRunner` over any `Provider`. `Harness::machine` hands the state machine to
callers who want to drive it step by step. This is also the programmable loop: an
`Operation` placed ahead of the tools can approve, rewrite or refuse what the model asked for.

**Cost.** oi-kit is async (tokio) and brings about 315 crates, and it is a two-day-old alpha
pinned to an exact version. That weight is confined to the top crate; `corduroy-core` stays
synchronous and dependency-free.

**Left out.** Real providers (`oi-kit-providers`), MCP extensions, compaction, tool approval
and sub-agents. All exist as oi-kit operations and slot into `Harness::with_operation`.

## Capabilities

The plugin world in `wit/corduroy.wit` imports four host interfaces and nothing else. There is
no WASI in the linker, so a plugin has no clock, environment, filesystem or network of its own.

| Capability | Grants | Scoped by |
| --- | --- | --- |
| `fs-read` | read files, list directories | absolute path |
| `fs-write` | create, replace and delete files | absolute path |
| `kernel` | run code in a host kernel | kernel name |
| `events` | publish to the event stream | topic |

- **Declared by imports.** A plugin's requirements are its component's imports, which the
  toolchain derives from the host functions the code actually calls. `WasmPlugin::requires`
  reports them; there is no separate manifest to fall out of step.
- **Granted by the host.** `WasmPlugin::grant(capability, scope)`. A scope is allow and deny
  globs; deny wins. `Scope::none()` links the interface but lets nothing through.
- **Enforced twice.** At load time only granted interfaces are linked, so a plugin missing a
  grant is never instantiated. At call time every host function resolves its resource (paths
  are made absolute and symbolic links followed first) and checks it against the scope; a
  refusal is returned to the plugin as `denied` and logged as `capability/denied`.
- **Metered.** Each call into a plugin has a fuel budget and instance memory is capped.

A new capability is a new WIT interface, a `Host` implementation, and an entry in
`CAPABILITIES`. Sandboxes layer on top of this: a kernel that runs in a container is still
just a kernel name in a scope.

## Open questions

- **Grant files.** Grants are built in code. A declarative form (Zed's `extension.toml`
  capabilities and Tauri's capability files are the closest prior art) would let users review
  and edit what each plugin may do.
- **WebAssembly gates.** WebAssembly plugins can observe events but not veto a tool call.
  Giving the world an `on-tool-call` export that returns allow or deny would put Pi-style
  gates and approval policies in plugins too.
- **Operations from plugins.** Loop steps are native `Operation`s. Letting plugins contribute
  steps needs an async-friendly, effect-returning export.
- **Blocking.** Tool calls run on blocking threads, but event listeners run inline on the
  thread that emitted, including inside the async loop. Slow listeners would need a queue.
- **Guest SDK.** Each plugin repeats the same `generate!` and error-mapping boilerplate; a
  small shared guest crate would remove it, and a second-language example would prove the
  "any language" claim.
