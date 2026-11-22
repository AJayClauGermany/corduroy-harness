# Design

corduroy-harness is a prototype of one idea: an agent harness in which nearly everything is a
plugin, plugins can be WebAssembly, and a WebAssembly plugin can do only what it was granted.
It takes one mechanism from each of six existing harnesses. This document records what each
of those mechanisms is (checked against the projects' sources in November 2026), what was
borrowed, where it lives here, and what was left out.

**Why a Rust core and WebAssembly plugins.** The core is meant to be fixed: compiled, small,
and not something an agent or a plugin can change. It holds the security boundaries (the log,
the capability checks, the gates) and the contract plugins are written against. Plugins are
the part that adapts: they can be added, rewritten and removed, by people or by the agent
itself, without the core moving. The aim is that whatever the plugins become, the same core
is underneath and still enforcing the same rules.

The second half records where the design is going. The core is being hardened before any
further plugins are written, so that plugins build on a surface that does not shift under
them. Sections and table rows marked *planned* describe that target, not the code as it is.

## The shape

```
                 ┌───────────────────────── corduroy-harness (src/) ─────────────────────────┐
                 │  Harness: oi-kit StateMachine over [ops, prompt, gates, tools, limit, LLM]  │
                 │  Journal: effects ──► events            PluginTools: registry ──► oi-kit   │
                 └───────────────┬───────────────────────────────────────┬───────────────────┘
                                 │                                       │
        ┌──────── corduroy-core (no dependencies) ────────┐   ┌──── corduroy-wasm ────┐
        │ App / Context / Plugin   (fibers, effects)      │   │ WasmRuntime, WasmPlugin │
        │ Grants / Scope           (capabilities)         │◄──┤ linker = granted WIT    │
        │ EventStream, Log, SessionId (commits + pub/sub) │   │ interfaces only         │
        │ Tools, Prompts, Kernels, Gates (registries)     │   └───────────┬─────────────┘
        └─────────────────────────────────────────────────┘               │ wit/corduroy.wit
                                                              ┌───────────┴─────────────┐
                                                              │ plugins/: pi-tools,      │
                                                              │ skills, approval (wasm32)│
                                                              └──────────────────────────┘
```

The core knows nothing about WebAssembly or models. The WebAssembly host knows nothing about
models. Only the top crate depends on oi-kit. `corduroy-jsonl`, not drawn, is a store for the
event stream: it implements the core's `Log` trait over a file.

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

See [Durability](#durability) for what is adopted.

### OpenHands: event stream and programmable loop

**What it is.** OpenHands V0 was a threaded pub/sub `EventStream` of actions and observations
with `source` and `cause`. The V1 [SDK](https://github.com/OpenHands/software-agent-sdk) made
the events immutable and typed, the agent stateless, and the conversation a *view* over an
append-only log, with condensation as an event rather than a rewrite.

**Here.** `corduroy_core::event` is an append-only log with synchronous, ordered delivery, and
every event has a source, an optional cause and, if it belongs to one, a session.
`src/session.rs` makes the conversation a projection of it: the `Journal` turns the effects
of one step of the loop into one commit of `message`, `tool/call` (caused by its message),
`tool/result` (caused by its call) and `usage` events, and `Harness::resume` rebuilds every
session from a replayed stream. Plugin lifecycle, capability denials and plugin-defined
topics share the same stream, so plugins and the model see one history. The stream writes
each commit to a `Log` before showing it to anyone; `corduroy-jsonl` is the store on disk.
See [Durability](#durability).

**Left out.** Condensation, and a security analyser and confirmation policy.

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

The plugin world in `wit/corduroy.wit` imports eleven host interfaces and nothing else. There
is no WASI in the linker, so a plugin has no clock, environment, filesystem or network of its
own beyond what these give it.

| Capability | Grants | Scoped by |
| --- | --- | --- |
| `fs-read` | read files, list directories | absolute path |
| `fs-write` | create, replace and delete files | absolute path |
| `kernel` | run code in a host kernel | kernel name |
| `events` | publish to the event stream | topic |
| `log` | diagnostic lines, recorded as `plugin/log` under the plugin's name (16 KiB each) | level: `debug`, `info`, `warn`, `error` |
| `clock` | wall-clock time; a monotonic clock that starts with the instance | clock: `wall`, `monotonic` |
| `random` | bytes from the host's secure generator (64 KiB per call) | source: `secure` |
| `tools` | list and call tools other plugins registered, through the gates | tool name |
| `state` | a durable key-value store of the plugin's own | key |
| `service` | call a service another plugin provides | service name |
| `plugins` | load and unload WebAssembly plugins as its children, granting no more than it holds | the loaded plugin's name |

Two limits hold whatever a grant says. **Protected paths** (the host binary, the compiled-code
cache and whatever the host adds with `WasmRuntime::protect`, such as the event log and grant
files) are out of reach of `fs-read` and `fs-write`. A kernel runs code the core cannot see
into, so protecting paths from a kernel is the kernel's job (a sandboxed kernel). And
**cascades end**: a plugin is not called for an event more than `Limits::cascade` causes
deep (16 by default); events a plugin raises while handling one name it as their cause, so
plugins setting each other off are measured and stopped, with `plugin/cascade` recorded.

Every capability is scoped, even where there is one obvious resource, so that `Scope::none()`
keeps meaning "linked but lets nothing through", which is how a script-host automation is
denied a capability its interpreter imports.

- **Declared by imports.** A plugin's requirements are its component's imports;
  `WasmPlugin::requires` reports them, and there is no separate manifest to fall out of step.
  For Rust the toolchain derives the imports from the host functions the code actually calls.
  JavaScript and Python components import every interface of the world they were built
  against, used or not, so there the author declares by choosing a world (see
  [Guest languages](guest-languages.md)).
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
[Plugins extending plugins](#plugins-extending-plugins)).

| Primitive | Status | Target |
| --- | --- | --- |
| Plugin tree (`App`, `Context`, effects, `inject`) | built | Built for WebAssembly: a call that traps kills the instance, and the next call makes a new one and activates it again, keeping the plugin's registrations; after `Limits::restarts` restarts (default 3) the next trap stops the plugin until it is reloaded (`plugin/trapped`, `plugin/restarted`, `plugin/stopped`). Still to do: the same limit in the plugin tree itself, so a stopped plugin shows as suspended in `App` rather than only refusing calls. |
| Event log | built | A fallible `Log` trait. A step's events are one atomic commit, stored before any listener or the model sees them. The core has the trait and the in-memory stream; stores are separate crates. |
| Sessions | built | Many sessions, each with an id and an optional parent. Every event of a run names its session; events about the plugin tree name none. Still to do: events a plugin emits while serving a tool call do not carry that call's session. |
| Tool lifecycle | partly built | Built: `tool/call`, `tool/start` and `tool/result` are each committed as they happen, per call; `ToolSpec::replay_safe` says whether a call that was running at a crash may be repeated, and `read_only` whether it changes anything. Still to do: results as content blocks rather than one string, progress and cancellation. |
| Gates | built for `tool/call` | A fourth registry. A gate is asked at a named decision point before something happens and answers allow, deny or ask; the strictest answer wins and is recorded. Reachable from WebAssembly through the `gate` export. Still to do: more decision points (`plugin/load` first). See [Events, gates and hooks](#events-gates-and-hooks). |
| Waits and signals | built for a gate's ask | A run whose tool call a gate asked about waits for `Harness::answer`; the ask and the answer are events, so the wait survives a restart. Still to do: the general form, a step that waits for any named signal (a child session finishing, a timer). |
| Services | built | A fifth registry, `Services`: calls between plugins in JSON text, with the caller's name, across the WebAssembly boundary in both directions. See [Plugins extending plugins](#plugins-extending-plugins). |
| Capabilities | eleven | The mechanism is settled; the set grows (below). |
| Kernels | built | The trait and registry stay. `ShellKernel` is an implementation, not a primitive, and moves out to a plugin crate. |
| Conversation projection | messages only | Understands a `compaction` event (a summary and the first message kept). Producing the summary is a plugin's job. |
| Model access | constructor argument | A `provider` service that a plugin supplies, so the model is swappable like everything else. |
| Versioning | WIT package version, log schema version | Built: a schema version in each log's header. Still to do: a version on each plugin's stored state, and a rule for what a minor version of the world may change. |

Capabilities to add, each a WIT interface with a scope:

| Capability | Grants | Scoped by | Needed by |
| --- | --- | --- | --- |
| `llm` | one model request | model name | compaction, summaries, the skills reviewer |
| `session` | start a child session and await it | recipe or agent name | sub-agents |
| `log-read` | read events of other sessions | session and topic | chat recall |
| `http` | make a request | host | web fetch, remote MCP |
| `secret` | read a named credential | secret name | providers, MCP servers |

Two rules follow from adding these:

- **Secrets never enter the log.** A secret is handed to a plugin as an opaque value, and host
  functions that take one (an HTTP header, say) record that it was used, not what it was.
- **Time and randomness are host calls.** A WebAssembly plugin has no clock of its own, so
  given the same host-call results it computes the same thing. That is what makes
  [replaying a plugin](#durability) possible, and WASI stays out of the linker to
  keep it true. The guest-language spike confirmed it for JavaScript and Python, with a
  catch: built without WASI their clocks trap or stand still and their random numbers repeat,
  so `clock`, `random` and `log` are needed as soon as either language is.

## Events, gates and hooks

What other harnesses call hooks are two different things here, and both are plugins.

- **Reacting is an event.** Events stay fire-and-forget: the emitter does not know who
  listens and gets nothing back. A hook that observes (log this, count that, nudge the
  prompt) is a plugin with a subscription. This keeps the log a plain record that replays
  without re-running anyone's decision.
- **Deciding is a gate.** Where the loop needs an answer before it proceeds, it asks the
  gates registered for that decision point, in a fixed order, and the strictest answer wins
  (deny over ask over allow). The outcome does not depend on which plugin loaded first. The
  decision itself is then recorded as an event.

The core defines a small, fixed set of decision points (`corduroy_core::POINTS`), starting
with `tool/call`. Likely additions are `plugin/load`, the user's input and the request about
to be sent to the model, each added when a plugin needs it. A WebAssembly plugin that lists
a point the core does not define is refused at activation. A plugin that wants to ask other plugins a question of its own does
so through a [service](#plugins-extending-plugins), not through the event stream.

A deny or an ask always carries a reason. A denied tool call becomes an error tool result
holding that reason, so the model learns what was wrong and can try again, as with any other
tool failure.

**How `tool/call` is gated (built).** A loop step, `GateOperation`, sits between the prompt
and the tools step. For each call of the model's message it asks the gates once, with
`{"session", "id", "tool", "input", "read-only"}`, and stores the decision as `gate/decision`
in that step's commit, so the decision is in the log before the tool can start. An allowed
call goes on to the tools step; a denied one is answered with an error result. A call a gate
asked about makes the whole message wait: none of its calls runs, the run stops with
`Status::Waiting` and no `turn/end`, and the session refuses a new prompt.
`Harness::answer(session, call, Answer)` stores a `gate/answer` and, once nothing else is
waiting, takes the run on; a declined call goes to the model with the person's reason.
`continue_run` leaves a waiting run alone and does not count it as a restart, so a run can
wait across any number of process lifetimes. With no gate registered nothing is asked or
recorded, and the loop is as it was.

- A gate that cannot answer fails closed: a WebAssembly gate that traps denies.
- Gates are asked in name order and only the first gate to give the strictest kind is
  recorded, so the reason the model or the person sees comes from one gate.
- Remembering an answer ("always allow this tool") is the gate plugin's business: it can
  subscribe to `gate/answer`.

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

## Durability

The oi-kit state machine already has the right shape: an operation reads the persisted
conversation and returns effects for the host to persist. One step is therefore one commit.
The first five points are built; the last two are *planned*.

- **Commit, then deliver.** `Journal::apply_effects` turns a step's effects into one batch
  and hands it to `EventStream::commit`. The batch is written to the `Log`; only then is it
  added to the stream, are listeners called and is the next step allowed to start. A refused
  write closes the stream, since the log may hold part of the batch, and is a broken
  pipeline: `Harness::run` returns `HarnessError::Log`. Emitters with nobody to report to
  (plugin lifecycle, events a plugin queued) drop the error, because the next commit of the
  run fails with the same one.
- **Stores are plugins of the log.** The core carries the `Log` trait and nothing that
  touches a disk. `corduroy-jsonl` is the first store: a header line with the schema
  version, then one line per commit, written with one `write` and synced. An unfinished last
  line, which is all a crash can leave, is cut off when the file is opened, and the file is
  locked because a log has one writer. *Changed from the plan:* the JSONL store was to live
  in the core, std only. Reading it back needs a JSON parser, and a hand-written parser on
  the path every event takes is not something the core should own, so the store is a crate
  of its own with `serde_json`, like SQLite would be.
- **Sessions are in the log.** A session begins with `session/start`, which records its
  parent, and each session's conversation is rebuilt from its own events. The core knows
  only `SessionId`; which sessions exist and how they relate is a projection the harness
  makes (`Harness::start_session`, `run_in`, `session_list`).
- **Tool intent first, one call at a time.** The model's message and its `tool/call` events
  are one commit, stored before the tools step starts. Each call then leaves a trail of
  its own: `tool/start` is committed just before the tool runs and `tool/result` as soon as
  it returns, each a commit, and only then does the message carrying the step's results go
  to the model. oi-kit's tools step runs every call of a message and returns the results
  together, and nothing in it splits that up; but every call passes through our
  `ToolProvider` (`PluginTools`), which is where the two events are written, so the step
  itself is still oi-kit's and the conversation keeps one result message per step.
- **Continue, not only resume.** A run is *unfinished* from its first message to its
  `turn/end`, and a session with an unfinished run refuses a new prompt.
  `Harness::continue_run` picks the run up without one, and treats each unanswered call by
  how far the log says it got:

  | The log has | Then |
  | --- | --- |
  | `tool/result` | The result is handed to the model from the log. The tool is not run again. |
  | no `tool/start` | The tool never ran. It runs now, like any other call. |
  | `tool/start` only | It may or may not have run. It is made again if its tool is `replay_safe`; otherwise the harness records an `interrupted` error result and the model recovers, as with any other tool failure. |

  Only the call that was actually running when the process died is ever in doubt. Results
  that do not come from a plugin's tool (a custom operation refusing a call, a call nobody
  handles) are still recorded with their message, as before.
- **Crash loops.** Each `continue_run` of an unfinished run is a `run/restart` event; past
  `Harness::with_restart_limit` (default 5) the run ends as `Status::Failed`, recorded in the
  conversation, and the session takes prompts again.
- **Plugin state** (built). Replayed events are not delivered to listeners, so state a
  plugin builds from events is lost on restart. A plugin keeps durable state through the
  `state` capability instead: each write is a `state/set` or `state/remove` event from the
  plugin, and the store is what those events add up to, read when the plugin is applied.
  State is explicit so that it is visible in the log and bounded: restoring a plugin reads
  its current keys, not its history. The writes of one call into the plugin are kept only if
  the call returns (a trapped call's writes are dropped) and are committed with the call's
  other events when it returns. *Changed from the plan:* writes were to join the commit of
  the loop step that caused them; they are committed when the plugin's call returns instead,
  which is the same commit whenever the plugin was called from outside the loop and, inside
  it, at most one commit apart. Values survive a restart of the plugin (they are in the
  host, not in its memory), a reload and a new process. **Decided against, for now:**
  journaling every host-call result and rebuilding a plugin by replaying them. It needs
  nothing from the plugin author, but the log grows with every file read and recovery slows
  as a session ages. It can be added later without changing `state`.
- **Kernel state** (*planned*). A kernel's processes do not survive a crash. The `Kernel`
  trait gains optional `snapshot` and `restore` (Prime Agent has both); a kernel without them
  reports on resume that its state was lost, as a timeout does today.

## Plugins extending plugins

Native plugins can already do this: one plugin calls `ctx.provide` with a service, which may
be a `Registry` of its own, and others list it in `inject` and register into it. Unloading
the provider suspends its dependents. Nothing in that path knows about tools or prompts, so a
plugin-defined extension point is as good as a built-in one.

Typed values cannot cross the WebAssembly boundary, so for WebAssembly plugins (built):

- A fifth registry, `Services`, of things called by name with a method and a payload, JSON
  text by convention, and told the caller's name. Native and WebAssembly plugins both
  provide and call them.
- `plugin-info.provides` lists the services a WebAssembly plugin answers, through the
  `handle-call` export (which receives a `service-call` record: caller, service, method,
  payload). A `service` capability, scoped by service name, lets a plugin call them.
- *Changed from the plan:* `plugin-info` was also to gain `inject`, so the plugin tree would
  order WebAssembly plugins by the services they use. A plugin's info is only known once it
  has been activated, which is after the tree has decided whether to activate it, so a call
  to a service nobody provides fails at the time of the call instead (`unknown`).
- **Never re-entered, never deadlocked.** A call that would enter a plugin already running
  further up the same call (a plugin calling its own tool or service, or A calling B calling
  A) is refused at once. A call from inside a plugin to one busy on another thread waits at
  most `Limits::nested_wait` (30 s by default), so two chains that wait on each other give up
  rather than hang. Only a call from outside every plugin waits for as long as it takes.
- **A gate does not make tool calls.** A plugin's own tool calls pass the gates, so a plugin
  that gated `tool/call` would be asked about its own call while making it. A plugin that
  lists `tool/call` in `gates` and imports `tools` is refused at activation. The rule is
  checked from the component's imports, so it holds without trusting the plugin.
- **Authority does not travel.** A plugin handling a call acts with its own grants, so a
  service is a deliberate grant of some of its power to callers. The host passes the caller's
  name with every call, and the `service` scope decides who may call what. A plugin that
  re-exports the filesystem to all callers is a misconfigured grant, visible in one place.

**Tools from plugins.** The `tools` capability lets a plugin list and call the tools the
model can (scoped by tool name), which is what automations and code mode need. Each call is
asked of the gates with `caller` set to `plugin:<name>`, so a policy can treat plugins
differently from the model; a gate's ask becomes a denial, since a plugin's call cannot wait
for a person. The call leaves `tool/call`, `gate/decision` and `tool/result` events with the
plugin as their source, committed when the plugin's own call returns. These calls are not
durable intent: they carry no session yet, and after a crash the plugin's own call is what
is repeated or not.

The built-in registries (`tools`, `prompts`, `kernels`, `gates`) are the cases where the loop
itself is the consumer. A plugin that wants its own extension point (an MCP plugin accepting
server definitions, a skills plugin accepting skill sources) provides a service.

## Automations

The purpose of the plugin system is that the agent can adapt the harness to its task: having
done something by hand, it writes an automation that does it from then on without a model
in the loop. This extends Hermes's idea from skills (text the model reads) to plugins (code
the harness runs). An automation is a WebAssembly plugin with three parts:

| Part | Provided by |
| --- | --- |
| A trigger | An event subscription (built). Time-based triggers are events too, emitted by a native `scheduler` plugin, since a plugin cannot wake itself. |
| An action | Host capabilities, and the `tools` capability to call other plugins' tools. Such calls pass through the gates and are logged like the model's, with the plugin as their source. |
| Optionally, the model | The `llm` capability for one request, or `session` to start a run. |

What the core must guarantee, because the author is now the agent (built):

- **No amplification.** A plugin holding the `plugins` capability grants a plugin it loads
  each requested scope *within* its own (`Scope::within`: a resource must be in both), so
  never more than it holds. A capability it does not hold at all it can grant only with an
  empty scope, which is how a child is linked to an import it may not use. The child's path
  must be one the loader can read with `fs-read`, and its name one the loader's `plugins`
  scope reaches.
- **Loading is a decision point.** `plugin/load` is asked of the gates with the loader, the
  child's name, the SHA-256 of its component, the capabilities it imports and the grants
  requested; the decision is recorded as `gate/decision` and the load as `plugin/requested`,
  both with the hash. A plugin that gates `plugin/load` may not import `plugins`, as with
  `tool/call` and `tools`. *Not yet:* a gate's ask is a denial here, as for a plugin's tool
  call, because the loader's call cannot wait for a person; approving a load waits on the
  general form of waits and signals.
- **The core's own files are out of reach.** The host binary and the compiled-code cache are
  protected from the start; the host adds the log, grant files and the plugin store with
  `WasmRuntime::protect`, and every plugin loaded through that runtime (children included) is
  refused them by `fs-read` and `fs-write`, whatever its grant says. A plugin cannot edit its
  own grants.
- **Cascades end.** See [Capabilities](#capabilities): a plugin is not called for an event
  more than `Limits::cascade` causes deep, and fuel bounds each call. *Changed from the
  plan:* the stream was to refuse a deep chain; it is the plugin delivery that refuses, since
  a refused commit closes the stream and native listeners are trusted.
- **Agent-written code is never native.** Native plugins are trusted, so only WebAssembly
  can be loaded through the `plugins` capability.
- **Loading from a running plugin.** A plugin's call cannot change the plugin tree, which it
  is part of. A load is checked, gated and compiled during the call, then queued through
  `App::requests` (`Requests`); the child already has its `FiberId`, is a child of the loader
  (disposed with it) and starts when the tree next settles: the harness settles it before
  every step of the loop, so a tool the child registers is offered to the model at the next
  step. *Not yet durable:* a loaded plugin lasts as long as its loader in this process; a
  loader that wants it back after a restart loads it again (from its `state`, say).

**Guest languages.** JavaScript/TypeScript and Python come first, since those are what other
harnesses' extensions are written in; the two existing Rust plugins are the reference, not
the expected norm. There are two ways to run them, and they suit different authors:

- *Compiled components* (`componentize-js`, `componentize-py`): each plugin is its own
  component. Best for plugins people write and ship.
- *A script host*: one interpreter component, instantiated once per automation with that
  automation's script as its configuration and its own grants. Nothing is compiled, so the
  agent can write an automation as a file, and the core still enforces each one's grants
  separately. Best for automations the agent writes.

Both were tried against the current world and both work; [Guest languages](guest-languages.md)
has the measurements. What it settled:

- **No WASI, and no change to the host.** Both toolchains can build a component that imports
  only `corduroy:plugin/*` (`disableFeatures` for JavaScript, `--stub-wasi` for Python). A
  default build imports WASI and is refused at load. Linking WASI interfaces that refuse
  everything is not needed.
- **A stubbed call is a trap.** `print`, `time.time()` and `open()` in Python, `fetch` and
  `performance.now()` in JavaScript take the instance down, and JavaScript's `Date.now()`
  returns the build time. Each language needs an SDK that routes these to `log`, `clock` and
  `random`, and turns an uncaught exception into a tool error.
- **These plugins import the whole world.** A plugin states what it needs by the world it is
  built against; the SDK generates that world from a list of capabilities. The script host
  imports everything and each automation is granted `Scope::none()` for what it may not use.
- **Compiled code is cached (built).** A runtime keeps each component it compiles by the
  SHA-256 of its bytes, so a second load in the process takes milliseconds (the script host
  for many automations compiles once). `WasmRuntime::cached(dir)` adds wasmtime's disk cache,
  and `parallel-compilation` is on. Measured on four cores: the JavaScript and Python
  components compile in 5.7 and 4.3 seconds (from 30 and 25), load again in the same process
  in 11 to 17 ms, and in a new process from the disk cache in 0.14 seconds. Each instance
  still holds 9 to 13 MB.

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
| Permission modes, per-tool permissions | `approval` (exists: `auto`, `smart`, `approve`, `chat`, allow and deny lists) | WebAssembly | done |
| Tool inspection, prompt-injection scan | `inspect` | WebAssembly | gates |
| Auto-compaction | `compact` | WebAssembly | `llm`, compaction event |
| Summon: `load`, `delegate` | `subagents` | WebAssembly | sessions, `session`, waits |
| Skills | `skills` (exists) | WebAssembly | `llm` for the review fork |
| Context files (`.goosehints`, `AGENTS.md`) | `hints` | WebAssembly | nothing |
| Todo | `todo` | WebAssembly | nothing (`state` is built) |
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

1. *Done.* `Log` trait, atomic commit before delivery, JSONL store, log header with a schema
   version.
2. *Done.* Sessions: ids, parents, a session on every event of a run.
3. *Done for durability:* intent before execution and a result per call (`tool/start`,
   `tool/result`), `replay_safe`, `Harness::continue_run` and the restart limit. *Moved to step 4,* where their first users are: `read-only` (gates),
   and content blocks, progress and cancellation (which change the tool calling convention
   and are better settled together with waits).
4. Gates, waits and signals; the rest of the tool lifecycle. Each export this adds to the
   world is one every guest language must implement, so check a new export against
   JavaScript and Python before it is fixed, async exports included (not yet tried).
   *Done:* the `Gates` registry, the `tool/call` decision point, the `gate` export (checked
   from JavaScript and Python, see [Guest languages](guest-languages.md#the-gate-export)),
   `read-only`, and waiting for a person's answer to an ask, with the `approval` plugin as
   the first gate. *Still to do:* general waits and signals, content blocks, progress and
   cancellation.
5. Services across the WebAssembly boundary; `state`; `tools`. Also `log`, `clock` and
   `random`, brought forward from step 7 because JavaScript and Python plugins cannot do
   without them, and re-instantiating a trapped plugin under the restart limit. *Done:*
   `log`, `clock`, `random` (checked from JavaScript and Python, see
   [Guest languages](guest-languages.md#host-calls-log-clock-and-random)) and restarting a
   trapped WebAssembly plugin, then services, `state` and `tools`, with re-entry refused and
   gating kept apart from calling tools (all checked from JavaScript and Python, see
   [Guest languages](guest-languages.md#between-plugins-tools-state-and-service)).
6. *Done.* Loading plugins safely: `plugins` with no amplification, the `plugin/load`
   decision point, protected paths, the cascade limit. With it, the compiled-code cache: a
   component compiled once per process, wasmtime's `cache` and `parallel-compilation`
   features switched on, and the cache directory among the protected paths. Checked from
   JavaScript and Python, see [Guest languages](guest-languages.md#loading-plugins).
7. The remaining capabilities, each when its first plugin needs it; `provider` as a service;
   `ShellKernel` out of the core.
8. Guest languages. *Spike done early:* one JavaScript and one Python plugin ran against the
   world, which settled the WASI question (build without it) and showed the script host
   works in both; see [Guest languages](guest-languages.md). *Still to do,* once steps 5 and
   6 have landed: an SDK per language (bindings, `print` and clocks routed to host calls,
   exceptions as tool errors, a world generated from a capability list, the build flags),
   the script host as a shipped component, and one example plugin per language built in CI.
9. Shipped plugins, MCP first.

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
- **Listeners and re-entry.** An event a native tool emits while a WebAssembly plugin's
  call is in progress is delivered at once, and a listener in that same plugin is refused
  (it is running), so it misses the event. Events plugins raise themselves are queued and
  unaffected. A per-plugin queue of undelivered events would close the gap.
- **Exports every guest must write.** Each export of the world is mandatory: the
  JavaScript and Python toolchains refuse to build a plugin without `gate`, and Rust's
  bindings will not compile. Plugins that guard nothing carry a stub. The guest SDKs should
  supply defaults, or generate the world without exports a plugin does not use.
- **Blocking.** Tool calls run on blocking threads, but event listeners run inline on the
  thread that emitted, including inside the async loop. Slow listeners would need a queue.
- **Guest SDK.** Each plugin repeats the same `generate!` and error-mapping boilerplate; a
  small shared guest crate would remove it. For JavaScript and Python an SDK is required
  rather than nice to have (see [Guest languages](guest-languages.md)).
- **Durable loads.** Plugins loaded by a plugin are not reloaded after a restart unless their
  loader does it. A plugin store (components by hash, with the grants they were loaded with)
  would let the host do it, and is also where grant files come in.
- **Approving a load.** `plugin/load` cannot ask a person yet; see Automations.
- **Shipping precompiled plugins.** wasmtime's cache removes the cost of every load but the
  first on a machine. Removing that one too means distributing compiled code, which is read
  back with the `unsafe` `Component::deserialize`. Not needed now; if it ever is, that is the
  place for one documented exception in a crate of its own.
