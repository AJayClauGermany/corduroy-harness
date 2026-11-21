# Design

corduroy-harness is a prototype of one idea: an agent harness in which nearly everything is a
plugin, plugins can be WebAssembly, and a WebAssembly plugin can do only what it was granted.
It takes one mechanism from each of six existing harnesses. This document records what each
of those mechanisms is (checked against the projects' sources in November 2026), what was
borrowed, where it lives here, and what was left out.

The second half records where the design is going. The core is being hardened before any
further plugins are written, so that plugins build on a surface that does not shift under
them. Sections and table rows marked *planned* describe that target, not the code as it is.

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

**Pi Durable.** `@earendil-works/pi-durable` (`packages/durable`, marked experimental) stores
each step of a run so that the run survives a crash. It is a separate runtime with its own
loop, not an extension of Pi's agent core, which is the lesson taken here: durability is a
property of the loop and the log, so it belongs in the core. Its mechanisms:

- Every change is one atomic commit, and nothing is shown before its commit is stored.
- A tool call's intent is committed before the tool runs. After a crash the call runs again
  only if the tool declared `replay: "safe"`; otherwise the model receives an `interrupted`
  error result and decides what to do.
- Tasks can sleep or wait for a signal, and the wait survives a restart.
- A restart limit (default 5) turns a crash loop into an error.
- Storage is pluggable (memory, JSONL, SQLite) and single-writer.

See [Durability](#durability-planned) for what is adopted.

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

**The baseline.** Goose is also the measure for the plugins shipped with the harness: what a
user gets from Goose out of the box should be reachable here with shipped plugins alone. See
[Shipped plugins](#shipped-plugins-the-goose-baseline-planned).

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

## The core boundary

The core provides primitives and nothing built from them. Something belongs in the core only
if one of these holds:

1. **It is on a path every effect passes through**: the log, the capability check, a step of
   the loop. A plugin cannot enforce what other plugins can route around.
2. **It is a kind of thing plugins contribute**: a tool, a prompt part, a kernel, a gate.
3. **Plugins must agree on it to work together**: event shapes, session identity, the calling
   convention between plugins.

Everything else is a plugin, and a plugin may itself offer primitives to other plugins (see
[Plugins extending plugins](#plugins-extending-plugins-planned)).

| Primitive | Status | Target |
| --- | --- | --- |
| Plugin tree (`App`, `Context`, effects, `inject`) | built | Add a restart limit so a plugin that keeps faulting is suspended, not retried forever. |
| Event log | in memory | A fallible `Log` trait. A step's events are one atomic commit, stored before any listener or the model sees them. |
| Sessions | one, hardcoded | Many sessions, each with an id and an optional parent. Every event names its session. |
| Tool lifecycle | derived from messages | `tool/call` committed before the tool runs, `tool/result` after. `ToolSpec` gains `replay` (safe or not) and `read-only`. Results are content blocks, not one string; tools can report progress and be cancelled. |
| Gates | native `Operation` only | A fourth registry. A gate is asked at a named decision point before something happens and answers allow, deny or ask. Reachable from WebAssembly through a world export. See [Events, gates and hooks](#events-gates-and-hooks-planned). |
| Waits and signals | none | A step can end by waiting for a named signal (a user's answer, a child session finishing). The wait is an event, so it survives a restart. |
| Services | native only | Calls between plugins across the WebAssembly boundary, as a capability scoped by service name. |
| Capabilities | four | The mechanism is settled; the set grows (below). |
| Kernels | built | The trait and registry stay. `ShellKernel` is an implementation, not a primitive, and moves out to a plugin crate. |
| Conversation projection | messages only | Understands a `compaction` event (a summary and the first message kept). Producing the summary is a plugin's job. |
| Model access | constructor argument | A `provider` service that a plugin supplies, so the model is swappable like everything else. |
| Versioning | WIT package version | A schema version in each log's header, a version on each plugin's stored state, and a rule for what a minor version of the world may change. |

Capabilities to add, each a WIT interface with a scope:

| Capability | Grants | Scoped by | Needed by |
| --- | --- | --- | --- |
| `llm` | one model request | model name | compaction, summaries, the skills reviewer |
| `session` | start a child session and await it | recipe or agent name | sub-agents |
| `service` | call another plugin's service | service name | plugins extending plugins |
| `state` | a durable key-value store private to the plugin | (none: always its own) | todo, memory, any counter |
| `log-read` | read events of other sessions | session and topic | chat recall |
| `plugins` | load, unload and list plugins | plugin name | an extension manager |
| `http` | make a request | host | web fetch, remote MCP |
| `secret` | read a named credential | secret name | providers, MCP servers |
| `clock` | read the time | (none) | schedulers, timestamps in output |

Two rules follow from adding these:

- **Secrets never enter the log.** A secret is handed to a plugin as an opaque value, and host
  functions that take one (an HTTP header, say) record that it was used, not what it was.
- **Time and randomness are host calls.** A WebAssembly plugin has no clock of its own, so
  given the same host-call results it computes the same thing. That is what makes
  [replaying a plugin](#durability-planned) possible, and WASI stays out of the linker to
  keep it true.

## Events, gates and hooks (*planned*)

What other harnesses call hooks are two different things here, and both are plugins.

- **Reacting is an event.** Events stay fire-and-forget: the emitter does not know who
  listens and gets nothing back. A hook that observes (log this, count that, nudge the
  prompt) is a plugin with a subscription. This keeps the log a plain record that replays
  without re-running anyone's decision.
- **Deciding is a gate.** Where the loop needs an answer before it proceeds, it asks the
  gates registered for that decision point, in a fixed order, and the strictest answer wins
  (deny over ask over allow). The outcome does not depend on which plugin loaded first. The
  decision itself is then recorded as an event.

The core defines a small, fixed set of decision points, starting with `tool/call`. Likely
additions are the user's input and the request about to be sent to the model, each added
when a plugin needs it. A plugin that wants to ask other plugins a question of its own does
so through a [service](#plugins-extending-plugins-planned), not through the event stream.

A deny or an ask always carries a reason. A denied tool call becomes an error tool result
holding that reason, so the model learns what was wrong and can try again, as with any other
tool failure.

**Decided against, for now:** gates that rewrite a call's input and let it run, as Pi's hooks
can. A rewrite saves a model round trip and guarantees the outcome, but the model then
believes it ran something it did not, two gates can rewrite the same call differently (which
"strictest wins" cannot settle), and the log must hold both the requested and the executed
input. Its uses are covered elsewhere:

| Use of a rewrite | Covered by |
| --- | --- |
| Correcting a call | Deny with a reason; the model corrects it. |
| Timeouts, sandbox wrapping | The kernel, which applies them to everything it runs. |
| A person editing a call at the approval prompt | Deny, plus the person's instruction as a new message. |
| Keeping a credential out of a command | The `secret` capability: the host injects the value, so the model never holds it. |
| Stopping a sensitive value being read or sent | Capability scopes, or a gate that denies. |

**Decided against:** a Cordis-style `bail` dispatch mode, in which the first listener to
return a value decides. It is more general, but it gives events return values, and "first
wins" lets a permissive plugin loaded early pre-empt a strict one.

## Durability (*planned*)

The oi-kit state machine already has the right shape: an operation reads the persisted
conversation and returns effects for the host to persist. One step is therefore one commit.

- **Commit, then deliver.** `Journal::apply_effects` turns a step's effects into one batch.
  The batch is written to the `Log`; only then are listeners called and is the next step
  allowed to start. A failed write is a broken pipeline and returns `Err` from `Harness::run`.
- **Stores are plugins of the log.** The core carries the trait, the in-memory log and an
  append-only JSONL log (std only, so the core stays dependency-free). SQLite and anything
  else live in their own crates. A log has one writer.
- **Tool intent first.** `tool/call` is committed before the tool is invoked. On resume, a call
  with no result is run again if its tool is `replay: safe`; otherwise the harness records an
  `interrupted` error result and the model recovers, as with any other tool failure.
- **Continue, not only resume.** `Harness::resume` rebuilds state; a new `Harness::continue`
  picks up an unfinished run without a new prompt.
- **Plugin state.** Replayed events are not delivered to listeners, so state a plugin builds
  from events is lost today. A plugin keeps durable state through the `state` capability,
  whose writes join the commit of the step that made them. State is explicit so that it is
  visible in the log and bounded: restoring a plugin reads its current keys, not its history.
  **Decided against, for now:** journaling every host-call result and rebuilding a plugin by
  replaying them. It needs nothing from the plugin author, but the log grows with every file
  read and recovery slows as a session ages. It can be added later without changing `state`.
- **Kernel state.** A kernel's processes do not survive a crash. The `Kernel` trait gains
  optional `snapshot` and `restore` (Prime Agent has both); a kernel without them reports on
  resume that its state was lost, as a timeout does today.
- **Crash loops.** Each restart of an unfinished step is counted in the log; past a limit the
  run ends as `Status::Failed`.

## Plugins extending plugins (*planned*)

Native plugins can already do this: one plugin calls `ctx.provide` with a service, which may
be a `Registry` of its own, and others list it in `inject` and register into it. Unloading
the provider suspends its dependents. Nothing in that path knows about tools or prompts, so a
plugin-defined extension point is as good as a built-in one.

WebAssembly plugins cannot, because the world only exports tools, an event handler and a
prompt part. The target:

- `plugin-info` gains `provides` and `inject`, each a list of service names, so the fiber
  tree orders WebAssembly plugins the same way it orders native ones.
- A `service` interface: `call(service, method, payload) -> result<string, host-error>`, and
  a matching world export `handle-call`. Payloads are JSON text, like events.
- The call is queued or refused if it would re-enter a plugin that is already running, in
  keeping with the rule that a plugin is never called back while it runs.
- **Authority does not travel.** A plugin handling a call acts with its own grants, so a
  service is a deliberate grant of some of its power to callers. The host passes the caller's
  name with every call, and the `service` scope decides who may call what. A plugin that
  re-exports the filesystem to all callers is a misconfigured grant, visible in one place.

The built-in registries (`tools`, `prompts`, `kernels`, `gates`) are the cases where the loop
itself is the consumer. A plugin that wants its own extension point (an MCP plugin accepting
server definitions, a skills plugin accepting skill sources) provides a service.

## Sandboxing

Three different things go by this name, and they land in different places.

| Layer | Question it answers | Where |
| --- | --- | --- |
| Plugin isolation | What can plugin code do? | **Core** (`corduroy-wasm`): no WASI, granted interfaces only, scope checks, fuel and memory limits. Built. |
| Execution sandbox | What can a command the model runs do? | **Plugin**: a kernel implementation. A container, bubblewrap or Landlock kernel registers under a name like any other. |
| Policy | Should this call happen at all? | **Plugin**: a gate (approval modes, allow lists, injection scanning). |

The core supplies the choke points, not the sandbox. What it must guarantee:

- **Every side effect of a WebAssembly plugin is a host call checked against a scope.** This
  holds today and must hold for each capability added.
- **Process execution goes through a kernel.** No capability may start a process any other
  way, or an execution sandbox could be bypassed. A plugin that needs a subprocess (an MCP
  server over stdio) asks for one through a kernel, so the same grant and the same sandbox
  apply.
- **Native plugins are trusted.** A native plugin runs with the host's authority and no check
  applies to it. Anything that comes from a third party, or that the agent writes for
  itself, must be WebAssembly.

## Shipped plugins: the Goose baseline (*planned*)

Each row is a plugin, with the core primitive it waits on. Nothing here is built into the
harness.

| Goose feature | Plugin here | Kind | Waits on |
| --- | --- | --- | --- |
| Developer: `write`, `edit`, `shell`, `tree`, `read_image` | `pi-tools`, extended | WebAssembly | content blocks |
| MCP extensions (stdio, streamable HTTP) | `mcp` | native (`rmcp`) | kernels for subprocesses, `secret` |
| Providers | `providers` (`oi-kit-providers`) | native | `provider` service, `secret` |
| Permission modes, per-tool permissions | `approval` | WebAssembly | gates, waits |
| Tool inspection, prompt-injection scan | `inspect` | WebAssembly | gates |
| Auto-compaction | `compact` | WebAssembly | `llm`, compaction event |
| Summon: `load`, `delegate` | `subagents` | WebAssembly | sessions, `session`, waits |
| Skills | `skills` (exists) | WebAssembly | `llm` for the review fork |
| Context files (`.goosehints`, `AGENTS.md`) | `hints` | WebAssembly | nothing |
| Todo | `todo` | WebAssembly | `state` |
| Memory | `memory` | WebAssembly | nothing (scoped files) |
| Chat Recall | `recall` | WebAssembly | `log-read` |
| Summarize | `summarize` | WebAssembly | `llm` |
| Extension Manager | `plugin-manager` | WebAssembly | `plugins` |
| Code Mode | `code-mode` | WebAssembly + kernel | kernel-to-host bridge |
| Recipes and sub-recipes | a declarative plugin tree with grants | host configuration | grant files |

**Left out of the baseline.** Apps, computer control, the auto-visualiser and the tutorial
are tied to Goose's desktop application. The scheduler and ACP are front ends: they drive a
harness from outside and need nothing from the core beyond sessions and signals.

## Order of work

The core first, in the order its parts depend on each other. No new plugin is started until
the step it waits on is done.

1. `Log` trait, atomic commit before delivery, JSONL store, log header with a schema version.
2. Sessions: ids, parents, a session on every event.
3. Tool lifecycle: intent before execution, `replay` and `read-only`, content blocks,
   progress, cancellation. `Harness::continue` and the restart limit.
4. Gates, waits and signals.
5. Services across the WebAssembly boundary; `state`.
6. The remaining capabilities, each when its first plugin needs it; `provider` as a service;
   `ShellKernel` out of the core.
7. Shipped plugins, MCP first.

## Open questions

- **Grant files.** Grants are built in code. A declarative form (Zed's `extension.toml`
  capabilities and Tauri's capability files are the closest prior art) would let users review
  and edit what each plugin may do. Joined with a plugin list it is also Goose's recipe and
  Cordis's `cordis.yml`: one file naming the plugins of an agent and what each may do.
- **Scrubbing the log.** If the model does write a sensitive value into a tool call, the
  assistant message holding it is committed before any gate is asked, so neither a deny nor a
  rewrite keeps it out of the log. If that matters it is a property of the log (a filter
  applied to a batch before it is stored), not of gates.
- **Session tree.** Sessions are linear for now, with a parent link. Pi's sessions are a tree
  of entries, so one session can branch from any earlier point, which gives rewinding and a
  common starting point for several attempts. Because every event names its session and a
  session names its parent, branching can be added as "a new session whose parent is event N
  of another" without changing the log.
- **Operations from plugins.** Loop steps are native `Operation`s. Letting plugins contribute
  steps needs an async-friendly, effect-returning export.
- **Blocking.** Tool calls run on blocking threads, but event listeners run inline on the
  thread that emitted, including inside the async loop. Slow listeners would need a queue.
- **Guest SDK.** Each plugin repeats the same `generate!` and error-mapping boilerplate; a
  small shared guest crate would remove it, and a second-language example would prove the
  "any language" claim.
