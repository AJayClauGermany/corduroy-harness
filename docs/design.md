# Design

corduroy-harness is a small harness you can trust to build and maintain itself, with only the
capabilities you allow it, growing into the helper your own work needs. [What it is
for](#what-it-is-for) says for whom, and what follows from it.

The means is one idea: an agent harness in which nearly everything is a plugin, plugins can
be WebAssembly, and a WebAssembly plugin can do only what it was granted. It takes one
mechanism from each of six existing harnesses. This document records what each
of those mechanisms is (checked against the projects' sources in November 2026), what was
borrowed, where it lives here, and what was left out.

**Why a Rust core and WebAssembly plugins.** The core is meant to be fixed: compiled, small,
and not something an agent or a plugin can change. It holds the security boundaries (the log,
the capability checks, the gates) and the contract plugins are written against. Plugins are
the part that adapts: they can be added, rewritten and removed, by people or by the agent
itself, without the core moving. The aim is that whatever the plugins become, the same core
is underneath and still enforcing the same rules.

The harness has three tiers, from what changes least to what changes most: the **core**,
**extensions** (native plugins: trusted, compiled into the command, extending what the core
can do) and **add-ons** (WebAssembly plugins: granted capabilities, plugged in without a
rebuild). *Plugin* is the word for both of the last two. [Three
tiers](#three-tiers-core-extensions-add-ons) says what goes where, and [What is
stable](#what-is-stable) which contracts are meant to hold from here on.

The second half records where the design is going. Sections and table rows marked *planned*
describe that target, not the code as it is.

## What it is for

**A small harness you can trust to build and maintain itself, with only the capabilities you
allow it, growing into the helper your own work needs.**

**Who it is for.** Someone who has used several harnesses and is tired of wiring each one up:
installing the pieces, connecting them, writing what is missing. The harnesses that do more
of this themselves show too little of how they work to be controlled. This one is for the
place between: the person gives a goal and the limits, and the agent builds the structure
around the task (tools, automations, connections), keeps it, and uses it again. It is
written first for its author's own use. Wide adoption is not a goal of 1.0, and a question
that only serves adoption can wait.

**Growth is the point; trust and little effort are its conditions.** What the agent builds
is code, and code it has written takes the place of model calls it used to make: what was
once reasoned through each time is afterwards just run (see [Automations](#automations)).
The same tokens then go further, and a smaller model can do more, because less is asked of
it each time. So "the agent extends itself" is a claim of 1.0, and the first thing to be
demonstrated.

**Which property leads.** Three properties serve that, and the value is in having them
together:

| Property | What it is for here |
| --- | --- |
| An add-on can do only what it was granted | The reason growth can be allowed at all. The person approves *what a thing may do*, not how it works, so the agent can give itself only what the person would have given it. This one leads. |
| A plugin leaves nothing behind when unloaded | The undo. Something the agent built wrongly is unplugged, and it tries again from a clean state. |
| The run is durable | The safety net, and the account. When something goes wrong the run can be stopped, read, and recovered, in the same run or another; and what the agent did can be reviewed afterwards, by the person and by the agent ([What the record promises](#what-the-record-promises)). |

**What 1.0 is measured against.** Two uses, and every other answer is judged by them:

1. *An agent at a terminal that extends itself.* One person, one machine. The agent writes
   the add-ons, automations and connections its task needs, keeps them, and uses them again.
2. *One agent growing into several that cooperate.* Starting from the first use, the agent
   builds what it takes for more than one agent to work on a task, each with its own loop,
   its own tools and its own limits on what it can see, and they coordinate. It shows that
   the harness changes shape and not only gains tools, and it tests the capabilities: the
   agents are trusted to build what the goal needs because none can give itself more than
   it was allowed. The test-driven pair described below is the case.

Possible, and not promised by 1.0: an agent working unattended for days and woken by events,
a long-lived service holding many sessions for many people, and a device without an
operating system. Each follows from the design and is welcome as a trial of it. None
decides a question before 1.0.

**A harness to use, not a kit.** The primary thing is the agent a person sits at. That
others can rebuild the command with their own parts is how it is made, not what it is for.
What it should be able to do is reach *other* harnesses (one running in the cloud, another on
this machine) as plugins: each behind the same boundary as any plugin, able to exchange
messages with the rest and reachable through this harness's one interface, without having
the run of it. That is something the flexibility allows, and the agent should be able to
grow such a connection itself. A manager of many harnesses is not the product.

**Tools come over MCP; add-ons change the harness.** MCP is the standard other harnesses
share, so a tool is as welcome from an MCP server as from an add-on, and the agent may build
either. An add-on is for what a tool cannot be: a gate, a part of the prompt, a listener,
state in the record, a service, and the shape of the loop itself. The case to hold the design
to: two agents doing test-driven development, one writing tests without being able to see
the code, the other writing code without being able to see the tests, passing work back and
forth until it is done. Each has its own loop, its own tools and its own limits on what it
can see, and either may be another harness. Expressing that needs an add-on to be able to
define a loop and sessions to be able to differ; neither is possible yet. How sessions
differ is decided ([What an extension can reach](#what-an-extension-can-reach)), and how an
add-on defines a loop is still in [Open questions](open-questions.md) (E12).

**What is left out.** Nothing is refused for good. The test for anything proposed is whether
it serves the agent adapting itself, for this use; what does not is neither built in nor
promised. The command line is the front end, and enough. An editor (through a protocol
editors and agents share), a terminal interface, or a page in a browser showing what the
agent has built and been granted, may grow from it; none is a condition of 1.0.

## The shape

```
                 ┌───────────────────────── corduroy-harness (src/) ─────────────────────────┐
                 │  Harness: a loop of [operations, compact, gates, tools, waits, model]       │
                 │  Journal: effects ──► events      Provider ◄── corduroy-providers (oi-kit) │
                 └───────────────┬───────────────────────────────────────┬───────────────────┘
                                 │                                       │
        ┌──────── corduroy-core (no dependencies) ────────┐   ┌──── corduroy-wasm ────┐
        │ App / Context / Plugin   (fibers, effects)      │   │ WasmRuntime, WasmPlugin │
        │ Grants / Scope           (capabilities)         │◄──┤ linker = granted WIT    │
        │ EventStream, Log, SessionId (commits + pub/sub) │   │ interfaces only         │
        │ Message (the conversation's shape), call scope  │   └───────────┬─────────────┘
        │ Tools, Prompts, Kernels, Gates, Services,       │               │
        │ Models, Secrets, HttpClients (registries)       │               │
        └─────────────────────────────────────────────────┘               │ wit/corduroy.wit
                                                              ┌───────────┴─────────────┐
                                                              │ plugins/: pi-tools,      │
                                                              │ skills, approval (wasm32)│
                                                              └──────────────────────────┘
```

The core knows nothing about WebAssembly or models. The WebAssembly host knows nothing about
models. The harness speaks the core's `Message`; only `corduroy-providers`, which builds real
model providers, depends on oi-kit, and converts to and from its types at that boundary. `corduroy-jsonl`, not drawn, is a store for the
event stream: it implements the core's `Log` trait over a file.

## The six inspirations

[Where the ideas come from](explanation/inspirations.md) explains each of these at more
length, for a reader new to the projects.

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
A plugin's name is its identity (the source of its events, the key of its stored state, what
a service it calls is told), so the tree gives a name to one plugin at a time: a second
plugin of the same name is not applied until the first is disposed.

**Left out.** `ctx.isolate` (per-subtree service instances), config-driven plugin trees
(`cordis.yml`), the five event dispatch modes (only plain emit exists), and async `apply`.

### Prime Intellect: long-lived kernels

**What it is.** [Prime Agent](https://github.com/PrimeIntellect-ai/prime-agent) gives the model
essentially one tool, `python_repl`. The kernel is a subprocess speaking newline-delimited JSON;
one namespace persists across cells, with timeouts that interrupt and then kill, an output cap,
snapshot and restore, and a bridge that lets code in the kernel call back into the host.

**Here.** `corduroy_core::kernel`. `Kernel` is the trait (`exec(code, timeout) -> Cell`) and
`Kernels` the registry. Implementations live outside the core: `corduroy-shell` has
`ShellKernel`, a persistent shell (POSIX, or PowerShell on Windows) whose variables and working directory survive between cells. A cell that times out or
calls `exit` costs the kernel its state, and the error says so, as Prime's reset notice does.
WebAssembly plugins reach kernels through the `kernel` capability, scoped by kernel name; the
`bash` tool in `pi-tools` is a thin layer over it. A WebAssembly plugin is also a small kernel
in its own right: its instance memory lives as long as the plugin stays loaded.

**Left out.** A Python kernel, snapshot and restore, and the kernel-to-host bridge
(programmatic tool calling from inside a cell). Sub-agents are child sessions here (the
`subagents` plugin), not kernels. Kernels are not sandboxes: a shell kernel can
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

**Tool results.** A tool returns content blocks, text or images, as MCP tools do, so an
MCP server's image reaches the model and a plugin's tool can return one. Text is the
common case and stays a one-liner in every guest language: a string is one text block.

**Left out.** Fuzzy matching in `edit`, saving truncated `bash` output to a file, per-tool
prompt snippets, and the session tree. Pi's gate-style hooks have an equivalent in
[gates](#events-gates-and-hooks), which WebAssembly plugins can be, and in
`Harness::with_operation` for native code.

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
every event has a source, the time its commit was stored, an optional cause and, if it
belongs to one, a session.
`src/session.rs` makes the conversation a projection of it: the `Journal` turns the effects
of one step of the loop into one commit of `message`, `tool/call` (caused by its message),
`tool/result` (caused by its call) and `usage` events, and `Harness::resume` rebuilds every
session from a stream loaded from the log. Plugin lifecycle, capability denials and plugin-defined
topics share the same stream, so plugins and the model see one history. The stream writes
each commit to a `Log` before showing it to anyone; `corduroy-jsonl` is the store on disk.
See [Durability](#durability).

**Elsewhere.** Condensation is the `compaction` event, written by the `compact` plugin.
The security analyser and the confirmation policy are gates: the `inspect` and `approval`
plugins.

### Goose: a loop of steps over the persisted conversation

**What it is.** oi-kit ("Open Intelligence Kit") is the set of crates Goose is built from,
published as `oi-kit-*` from the [Goose repository](https://github.com/aaif-goose/goose).
`oi-kit-agent` unrolls the agent loop into a state machine: an ordered list of
`Operation`s, each of which looks at the persisted conversation and either declines or returns
effects for the host to persist; inference is the last step. `oi-kit-provider-types` has its
`Provider`, `Message` and `Conversation`, and `oi-kit-providers` the HTTP clients for
Anthropic, OpenAI, Ollama and the providers Goose knows.

**Here.** The idea, not the crate. The harness runs a loop of its own of the same shape, and
the loop itself is a part that can be swapped (see [The loop](#the-loop)). The standard one
(`StandardLoop`, `src/loops.rs`) tries at each step the host's operations, the operations
extensions registered (the `Operations` registry), compaction, the gates, the tools, waiting
for a signal, the turn limit and the model in order, and the first that does something is the
step; its effects become one commit. `Operation` is a small trait of the harness's own (`Next`: pass, continue,
stop), and `Harness::step` runs one step for a caller who wants to watch. The model is a
service (`src/provider.rs`): the loop asks whichever `Provider` is registered under its
provider name in the `Providers` registry, looked up on every request, so a `provider`
plugin supplies it and swapping the plugin swaps the model.

*Changed from the plan:* the harness used oi-kit's state machine, messages and provider
trait directly. That made oi-kit's `Message` the payload of every `message` event, so the
log, the plugins that read it and every extension depended on an alpha crate's
serialisation, and its `Operation` the extension point for loop steps. oi-kit-agent is 14,000
lines (an MCP client, skills, compaction, sub-agents, a malware check) of which the harness
used about a thousand, much of it overridden. The loop is now some 300 lines here; the
conversation is the core's `Message`; and oi-kit is confined to `corduroy-providers`, whose
`build` returns a harness `Provider` that converts at the boundary. That keeps what is worth
keeping, real and maintained HTTP clients for many providers (oi-kit-providers is 17,000
lines, its formats another 15,000), without letting their types past it. The harness library
lost about 60 crates of dependencies with it.

**Not taken from oi-kit.** Its MCP, compaction and sub-agent operations. Here those are
plugins over the core's own primitives (`corduroy-mcp` through a kernel, the `compaction`
event, child sessions), so that they are recorded, gated and unloaded like everything
else.

**The baseline.** Goose is also the measure for the plugins shipped with the harness: what a
user gets from Goose out of the box should be reachable here with shipped plugins alone. See
[Shipped plugins](#shipped-plugins-the-goose-baseline).

## Capabilities

The plugin world in `wit/corduroy.wit` imports sixteen host interfaces and nothing else. There
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
| `llm` | one completion from a model a provider serves, recorded as `llm/request` and `llm/response` with what it cost | model, as `<provider>/<model>` |
| `secret` | an opaque handle to a named credential, never its value | secret name |
| `http` | an HTTP request through the client registered for its scheme, with credentials written in by the host | host name |
| `log-read` | events of any session, by topic, in pages | `<session>:<topic>`, the session with `%` and `:` percent-encoded (empty for harness events) |
| `session` | which session the plugin is being called for (a tool call, a prompt part, a gate request, an event of the session), the tool call's id, and whether the run was asked to stop | session id |

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
  refusal is returned to the plugin as `denied` and logged as `capability/denied`. Calls
  that list (`tools.list`, `state.keys`, `log-read`) leave out what the scope does not reach
  instead of refusing, and `plugins.loaded` and `credential.name` only report what the plugin
  already holds.
- **Metered.** Each call into a plugin has a fuel budget and instance memory is capped.

A new capability is a new WIT interface, a `Host` implementation, and an entry in
`CAPABILITIES`. Whether something new is a capability of its own or a function of one that
exists is decided by what the grant already covers
([How the plugin world grows](#how-the-plugin-world-grows)). Sandboxes layer on top of this: a kernel that runs in a container is still
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
| Plugin tree (`App`, `Context`, effects, `inject`) | built | A name belongs to one plugin at a time, and never to another event source (`host`, `agent`, `user`, `environment`). Built for WebAssembly: a call that traps kills the instance, and the next call makes a new one and activates it again, keeping the plugin's registrations; after `Limits::restarts` restarts (default 3) the next trap stops the plugin until it is reloaded (`plugin/trapped`, `plugin/restarted`, `plugin/stopped`). Still to do: the same limit in the plugin tree itself, so a stopped plugin shows as suspended in `App` rather than only refusing calls. |
| Event log | built | A fallible `Log` trait. A step's events are one atomic commit, stored before any listener or the model sees them, and stamped with the time it was stored: one reading of the clock per commit, never earlier than the commit before. The core has the trait and the in-memory stream; stores are separate crates. |
| Sessions | built | Many sessions, each with an id and an optional parent. Every event of a run names its session; events about the plugin tree name none. Whatever is done for a session (a tool call, rendering a prompt part, asking a gate, delivering an event of the session) is done in its call scope (`corduroy_core::call`), so the events a plugin raises then carry the session, and a plugin can ask which session it serves (the `session` capability) and keep something per session: `todo` keeps a list for each, so a sub-agent's plan does not overwrite its parent's. A session may be started by another for a task (a child), and nests at most `MAX_DEPTH` deep. *Planned:* a session started with a model, a loop, instructions, grants and limits of its own, which whatever it starts cannot exceed ([What an extension can reach](#what-an-extension-can-reach)). |
| Tool lifecycle | built | `tool/call`, `tool/start` and `tool/result` are each committed as they happen, per call; `ToolSpec::replay_safe` says whether a call that was running at a crash may be repeated, and `read_only` whether it changes anything. A run can be stopped (`Harness::stop`): a running tool learns it through its call scope (`call::stopping`, the `session` capability's `stopping`), the calls not yet begun are answered as not run, and the run ends as stopped. A result is a list of content blocks, text or images (`ToolOutput`, `content` in the WIT), which reach the model as they are; `tool/result` records the result's text as `output` and, when it is anything but one block of text, the blocks as `content`, so a finished result is handed over from the log after a crash. Still to do: progress and cancellation, which need no change to this contract (see [Open questions](#open-questions)). |
| Gates | built | A fourth registry. A gate is asked at a named decision point before something happens and answers allow, deny or ask; the strictest answer wins and is recorded. Reachable from WebAssembly through the `gate` export. Decision points are declared: the core declares `tool/call` and `plugin/load`, an extension declares the points it asks at, and asking at an undeclared point is denied. Still to do: letting a person answer an ask at `plugin/load`. See [Events, gates and hooks](#events-gates-and-hooks). |
| Waits and signals | built | A run whose tool call a gate asked about waits for `Harness::answer`. In general, a tool call may wait for a named signal: whatever serves it says so through `corduroy_core::call::wait_for`, the call gets a placeholder and a `tool/wait`, the loop stops the run before the model is asked again, and a `signal` event (`Harness::signal`) brings the result. A child session's end is such a signal, which the harness raises itself. All of it is in the log, so a wait survives a restart. Still to do: a way for a plugin to raise a signal, and a driver that takes the run on; a timer is then an extension ([What an extension can reach](#what-an-extension-can-reach)). |
| Services | built | A fifth registry, `Services`: calls between plugins in JSON text, with the caller's name, across the WebAssembly boundary in both directions. See [Plugins extending plugins](#plugins-extending-plugins). |
| Capabilities | sixteen | The mechanism is settled; the set grows (below). |
| Kernels | built | The trait and registry, and `Kernel::spawn` for long-lived processes with pipes (MCP servers). `ShellKernel` is an implementation, not a primitive, and lives in `corduroy-shell`. |
| Conversation projection | built | Messages, and the `compaction` event: the messages before `keep_from` stay in the log and the conversation but are hidden from the model, which sees the summary in their place. The harness cuts only before the current turn; when, and the summary, come from the plugin providing the `compaction` service, asked by a loop step (`CompactOperation`). |
| Model access | built | The loop asks the provider registered under a name in `Providers`, on every request, so a plugin supplies the model and unloading it takes the model away. `Harness::new` registers its argument there for convenience. For plugins, the core's `Models` registry: one completion, synchronously, of a model named `<provider>/<model>`; a `provider` plugin serves its provider's models there. The loop and plugins fill in one request and read one reply, the core's (`ModelRequest`, `ModelReply`, with `Usage` and `StopReason`), and a model describes itself (`ModelInfo`). *Planned:* the general typed part, a cost on the usage, and listing models through `llm` ([What the core is made of](#what-the-core-is-made-of)). |
| Secrets and outbound requests | built | `Secrets` and `HttpClients` registries. The core checks and records; native plugins hold the values and send the requests, so an HTTP client is a plugin like a kernel is. |
| Versioning | WIT package version, log schema version | Built: a schema version in each log's header (2, which added the time; 1 is still read), and the rule for the world in [What is stable](#what-is-stable). Still to do: a version on each plugin's stored state, and the check of the world's version when an add-on is loaded ([How the plugin world grows](#how-the-plugin-world-grows)). |

**Child sessions, a service rather than a capability (built).** The plan was a `session`
capability to start a child session and await it. It became a service the harness
provides, `sessions` (`start` with a task), reached through the existing `service`
capability scoped to `sessions`, so the world did not grow an interface. A parent run waits
for its child in the log, as it waits for a person, rather than blocking a plugin's call
for as long as the child runs: `start` makes the call wait for the child's end, the harness
runs the child to its end and signals the parent, and `continue_run` picks the chain up
after a crash wherever it stopped. A gate asking in a child surfaces on the parent's
outcome, with the child's session on the ask.

## What the core is made of

Four things about the core's own shapes are decided here: what a model call is, what a model
says about itself, that whatever a plugin contributes is called and waited for, and that the
kinds of contribution are closed. The model call is built where it says so below: the one
request and reply, models named by their provider, and a model's description. The rest is
*planned*, and [Order of work](#order-of-work) lists what changes.

**A plugin asks a model as a function; anything that acts is a session.** Code uses a model
in one of two ways. As a function: content goes in and an answer comes back (summarise this,
is this change risky, which of these files matters), and the model cannot act. As an agent:
the model is offered tools, picks one, something runs it, the model sees the result, and so
on until the work is done. The second needs what the harness exists to give (the gates, a
trail for each tool call, a wait for a person, a stop, a resume), so it always happens in a
session the harness runs and never privately inside a plugin: `llm` offers a model no tools.

A session does not have to be started by the person. The agent or an add-on starts one
(`delegate` does, through the `sessions` service) within the limits the person set, and the
harness runs it and keeps its record. That fits what the agent builds for itself. A tool it
wrote is code that takes the place of model calls; where the tool needs a little judgement
it asks a model a question; where the work needs an agent, that is a session. The
[test-driven pair](#what-it-is-for) is then an add-on that starts two sessions and passes
work between them, not an add-on running two loops of its own. The cost is that an add-on
cannot make a new shape of loop out of bare model calls, so two things carry that weight.
What may differ between sessions is decided
([What an extension can reach](#what-an-extension-can-reach)); how an add-on says what a
session's steps are is still in [Open questions](open-questions.md) (E12).

**One description of a model call, in the core.** Whatever asks a model fills in the same
request and reads the same reply:

| | Holds |
| --- | --- |
| The request | The model, the system prompt, the conversation as `Message`s, and the tools offered. Only the loop offers tools. It can grow. |
| The reply | A `Message`, what the request cost, and why the model stopped. It can grow. |

Built: `corduroy_core::model` has `ModelRequest`, `ModelReply`, `Usage` and `StopReason`.
They took the place of the core's `Turn`, `Completion` and bare `String`, and of the
harness's `ModelRequest`, `ModelReply` and `Usage`, which moved into the core. What is added
later (a limit on tokens, a new kind of part) is added once and reaches the loop and plugins
alike. Two ways of asking stay, over the one description: the loop's `Provider`, which is
asynchronous and lives in the harness, and the core's `Model`, which is called and waited
for and is what `llm` reaches. Going from one to the other loses nothing, where it used to
keep only the text.

- *A plugin's call is recorded with its cost* (built). `llm/response` has the usage and the
  reason the model stopped, so a review of the log shows what the agent's own tools spent,
  and whether code it wrote did make the tokens go further. The loop's `usage` event has the
  reason too: `finished`, `limit` (cut off at the most it may write), `tool_calls` or
  `refused`, when the provider says. Which reasons there are in the end, and whether a
  reply cut off at its limit should end a run differently, is still open (G10).
- *A plugin's request carries what a message can*, images included. The core's request
  does; an add-on's is still text until the next point is settled.
- *How the request crosses the WebAssembly boundary* (a typed record, or the JSON a
  `message` event holds) is still open (E8). Until it is, the `llm` interface is as it was
  (text in, text out) and the host fills in the core's request from it.

**The grant is the one list of models** (built). `llm` is scoped by model, and that is the
only thing that says which models an add-on may ask. The `serve` key of a provider entry, a
second list that had to be kept in step with the grants, is gone: an entry of `Models` is a
provider, registered once under its name and asked for any of its models. A plugin names a model as
`<provider>/<model>`: the provider's name in the agent file, then the provider's own name
for the model, divided at the first `/`. Grants are written the same way, with the allow and
deny patterns every scope has, so "only these" and "all but this one" are both a grant. Two
providers that offer a model of the same name no longer collide.

**A model is anything that takes content and returns content.** A chat model is one kind.
An embedding model returns a vector; a decision model returns a yes or a no (is this text
an injected instruction), and it is the tool or the loop that asked which decides what to
do with it. The core does not list the kinds. The request is still messages, and the reply
is a message whose parts are whatever that model returns. For that, `Part` gains one general
kind, a typed value (what kind of data, and the data), so the core need not learn each new
kind of model; `Part` is already marked for growth. A vector database or a search index is
not a kind of thing in the core: it is a service an extension or an add-on provides, over an
embedding model if it wants one.

**A model describes itself** (built). The core has a small description that can grow
(`ModelInfo`), in which every field may be unknown: what the model takes (images, tools),
how much context it holds, how much it may write. What it returns joins them with the
typed part above. `Model` and `Provider` each have a method with a default that knows
nothing (`describe`). A provider fills in what it knows (`corduroy-providers` from oi-kit's
catalogue), the agent file states or corrects it for a model
(`[providers.<name>.models.<model>]`), and the loop and plugins read it in one place.
"Unknown" is an answer every reader has to cope with, since a model served on this machine
often reports nothing. `compact` starts from a share of the model's own context, three
quarters, which the harness passes on with each question to the `compaction` service, and
its fixed number is an override; one number for every session cannot be right once sessions
differ in their model.

- *The agent can see its models, if it is given the means* (planned). Through `llm`, a
  plugin lists the models its grant reaches, with their descriptions, as `tools.list` leaves
  out what a scope does not reach. The agent sees them only when an add-on that offers this
  is loaded. It waits on the form of E8, and on a provider saying which models it has.
- *A session keeps its model.* Choosing is for the next session or a child, not for the run
  under way. What else `session/start` carries is in
  [What an extension can reach](#what-an-extension-can-reach).
- *Rules about the choice are policy.* That a child may not run on its parent's model, or on
  one of the same family, is a gate's to say, at the decision point for starting a session
  (`session/start`, decided there and not yet built).
- *Cost is reported for each call, not described.* It comes with the usage, as far as the
  provider says, and goes into the log: the tokens read, the tokens written, those spent on
  reasoning (built, for the loop's calls and a plugin's), and what the call cost if the
  provider states it (planned: the unit it is written in is still open, G17). A figure that is absent means
  the provider did not say; zero means it said the call was free, as a model on this
  machine may. A change of price needs nothing: each call records what it cost when it was
  made.

**Call and wait is the core's contract, for good.** Everything a plugin contributes (a
tool, a service, a gate, a model, an HTTP client, a kernel) is called, and its caller waits
until it returns. Add-ons work this way whatever is decided: a WebAssembly guest is called
and returns, in every guest language. So the question only ever concerned extensions, which
are native and have threads.

- *A call that would take long does not hold its caller.* It says its result will come with
  a signal (`call::wait_for`) and returns; the run waits in the log, and the signal takes it
  on. A wait in the log survives a restart, which a wait in memory does not, and it is also
  the answer on a machine with one thread.
- *A signal resumes; an event is watched.* A signal takes a waiting run on and carries any
  outcome: a result, a failure, or just the next message from the other side. Progress (now
  on the third task) is an event: anything may listen, and nothing has to wait for it. An
  agent that wants to react part-way waits for the next message and not for the last one:
  it gets it, decides, and waits again. That is how two agents pass work back and forth.
- *An extension that holds a connection keeps a thread of its own*, as `corduroy-mcp` does.
  Asynchrony lives in the loop (`Provider`, `Loop`, `Operation`), which is stable and not
  frozen, and keeps growing there. No asynchronous form of a core trait is marked for
  growth, and a frozen contract gains no trait, so this is for good.
- *Something outside may speak first.* A pull request is opened, or a connected harness says
  something nobody here was waiting for. It arrives through an extension as an event, and
  starts a session or resumes one. Automations built on that may ship after 1.0; the way
  for an extension to raise a signal and to start a session has to exist before the freeze,
  and is decided: the `sessions` service records, and the host's driver takes the run on
  ([What an extension can reach](#what-an-extension-can-reach)).

This is the working answer, and one of the proofs [the freeze waits
on](#what-the-freeze-waits-on), another harness connected as a plugin, is what confirms it
or shows it wrong. If it is wrong the core changes, before 1.0.

**The kinds are closed at eight.** Tools, prompt parts, gates, services, models, kernels,
secrets and HTTP clients are everything a plugin, extension or add-on, contributes as far
as the core is concerned. An extension also contributes to the loop's own registries
(providers, steps, loops), which are stable and may gain more. After the freeze there is no
ninth: a new kind is a registry some extension provides, which an add-on reaches as a
service. The test for a ninth was whether it could not be made from the eight together with
sessions and events, and none of the candidates passed it:

| Candidate | Where it goes |
| --- | --- |
| Embeddings, classifiers and other models that are not chat | Models, as above. |
| Search, a vector database | A service. |
| Stored bytes (images and large results, which the log holds as text) | Not something plugins contribute. Where a store keeps a large payload is the store's business; whether a message may name bytes kept elsewhere is still open (C20). |
| Asking the person a question | Not a gate, which decides whether something may happen, and not a registry: a tool that asks and waits for the reply. It is in 1.0, and needs one agreed form before front ends are written: an event a front end must show, and a signal that carries the reply (D10). |
| Schedules and timers | An extension that raises signals ([What an extension can reach](#what-an-extension-can-reach)). |

## Three tiers: core, extensions, add-ons

A **plugin** is anything plugged into the core through `corduroy_core::Plugin` and
unplugged again: everything it registers is an effect, reverted when it is unloaded. There
are two kinds of plugin, extensions and add-ons, told apart by how they get in and what they
are trusted with, not by what they contribute. Underneath both is the core.

| Tier | What it is | Gets in by | Trust |
| --- | --- | --- | --- |
| **Core** | `corduroy-core` (the plugin tree, capabilities, the event stream, the registries); the WIT world (`wit/corduroy.wit`) and the WebAssembly host that enforces it (`corduroy-wasm`); and the loop (`corduroy-harness`: `Harness`, `Journal`, the built-in steps, `PluginTools`, the `Providers`, `Operations` and `Loops` registries, the `sessions` service; the loop that takes the steps is itself registered, `StandardLoop` by default) | is always there, and cannot be unloaded; the WebAssembly host only in a build with add-ons | the trusted base; changes rarely, as the contract the other two are written against |
| **Extension** | a native plugin: Rust compiled into the command | rebuilding the command with it; the agent file then turns it on | the host's authority |
| **Add-on** | a WebAssembly plugin: a component built against the WIT world | a `.wasm` file, plugged in at start or while the agent runs; nothing is rebuilt but the add-on | none beyond its grants |

The names say what each is usually for. An extension *extends what the core can do*: a new
kind of ability, such as a kernel, an HTTP client, a model, a decision point or a step of
the loop. An add-on *adds onto what is already there*, using only the abilities the core
and the extensions offer. That is the usual case, not the definition: `corduroy-mcp`
contributes nothing but tools and is still an extension, because it starts processes. The
definition is how a plugin gets in: **an extension needs the command rebuilt; an add-on is
dropped in.** Trust follows from it: whatever can be added without a rebuild is sandboxed,
and whatever holds the host's authority was compiled in by whoever built the binary.

- **Turning on an extension the command already has is configuration.** The shipped
  command has the providers and the kinds `shell-kernel`, `powershell-kernel`, `mcp`,
  `env-secret` and `http-client`; the agent file picks which are loaded (`[providers.*]`,
  `[kernels.*]`, `[mcp.*]`, `[secrets.*]`, `[extensions.<name>]`). Every entry but a
  provider is built from a kind of the command's `Standard`, so a command of one's own can
  swap any of them (secrets from a keychain, a sandboxed kernel) without touching the rest. A new *kind* of
  extension is Rust in a command of one's own (see [In the `corduroy`
  command](#in-the-corduroy-command) below). Native code is never loaded at run time.
- **An add-on is built on its own** (`cargo plugins`, `corduroy-js build`, `corduroy-py
  build`) and plugged in either at start, under `[[plugins]]` in the agent file with its
  grants, or while the agent runs, through the `plugins` capability (`plugin-manager` gives
  the model tools for it). A load while running is asked of the gates at `plugin/load`, is
  granted at most what its loader holds, and starts before the model's next step.

**Names older than these terms.** Several identifiers say *plugin* and reach add-ons only:
the `[[plugins]]` key of the agent file, the WIT package and world (`corduroy:plugin`,
`world plugin`), the `plugins` capability, the `plugin/load` decision point, the
`plugin/trapped`, `plugin/restarted` and `plugin/stopped` events, `WasmPlugin`, the
`plugins/` workspace and `plugin-manager`. They stay as they are: an add-on is a plugin, so
each is still true, and most are [stable contracts](#what-is-stable). Read them as
"plugins, of which only add-ons get here". Likewise "WebAssembly plugin" in this document
means an add-on, and "native plugin" an extension.

**Neither.** Some of what a running agent is made of is not a plugin at all:

- *Host configuration*: what the host hands the core while putting it together, rather than
  plugging in. The log's store (`corduroy-jsonl`'s `JsonlLog`, given to the `EventStream`),
  steps added with `Harness::with_operation`, protected paths (`WasmRuntime::protect`).
  None of it is an effect, and none of it is unloaded.
- *Implementations*: libraries that implement a core trait, such as `ShellKernel`,
  `UreqClient` and the providers `corduroy-providers` builds. The extension is the plugin
  that registers one (`shell_kernel`, `http_client`, `provider`).
- *The host*: `corduroy-cli`, which puts the core, its extensions and its add-ons together
  from an agent file.
- *What a plugin manages*: skills, scripts, memories, hint files. Data, not plugins.
- *What an extension reaches*: an MCP server's process, a model's API. Outside the harness.

An extension is not a second mechanism. It is a plugin (its registrations are effects, it
is unloaded like any other, it follows the services it injects) that happens to be native,
and so may do the things only native code can:

| An extension contributes | Through | Example |
| --- | --- | --- |
| An implementation of a core trait | the core's registries | `corduroy-shell` (a kernel), `corduroy-http` (the client behind `http`), `provider` (a model), a secret source |
| A service add-ons call | `Services`, reached with the `service` capability | an MCP extension accepting server definitions (the core's `sessions` service has the same shape) |
| A typed service or registry for other extensions | `ctx.provide`, `inject` | `Providers` |
| A decision point of its own | `Gates::declare`, then `Gates::decide` | asking the gates before a request leaves the machine |
| A step of the loop | the harness's `Operations` registry | a policy that answers a call before it runs; a compaction strategy |
| Tools, prompt parts, gates, event listeners | as any plugin does | `corduroy-mcp` |

That table is also the answer to *how extensions make add-ons easier to write*: an add-on
never gains authority from an extension, it gains something to ask. An add-on that needs
what no capability gives (a database, a browser, a queue) calls a service an extension
provides, and its grant names that service; an add-on that wants to veto something the core
knows nothing about gates a point an extension declared. Neither needs a change to
the core or to the WIT world.

What an extension cannot do is widen the add-on boundary. The capabilities are the imports
of the WIT world, and a new one is a change to the core (see
[Capabilities](#capabilities)), because the boundary is what the sandbox is made of: if
extensions could link host functions of their own, "what can an add-on do" would no longer
have one answer in one file. A service is the pressure valve: text in, text out, scoped by
name, with the caller's name attached.

**The standard extensions, and why each is one.** Each needs something no capability gives
an add-on, which is what earns it a place outside the sandbox. Anything that could be built
from the capabilities alone belongs in an add-on instead.

| Extension | Kind | Needs, natively | Could it be an add-on? |
| --- | --- | --- | --- |
| `corduroy-shell` | `shell-kernel`, `powershell-kernel` | processes, pipes, process groups | No: the `kernel` capability is how add-ons reach it. |
| `corduroy-http` | `http-client` | sockets and TLS | No: it is what the `http` capability sends through. |
| `corduroy-providers` | `[providers.*]` | async HTTP clients, credentials, streaming | Not yet: the loop asks a `Provider` for every step, and the world has no export to serve one and gains none in 1.x ([How the plugin world grows](#how-the-plugin-world-grows)). The route, if it is ever wanted, is a service an add-on provides over `http` and `secret`, which an extension registers as a provider. |
| `corduroy-mcp` | `mcp` | starting a server through `Kernel::spawn` and holding its pipes; a streaming HTTP client | Over stdio, no: no capability spawns. Over streamable HTTP, possibly, through `http` (one request at a time, no streams); not done. |
| `corduroy-std` | `env-secret` | the process environment | No: an add-on has no environment, and must never see the value. |

The loop's own built-ins are core tier, not extensions: `GateOperation` (every tool call
passes it), `CompactOperation` (the bridge that lets an add-on provide the `compaction`
service, since add-ons cannot contribute loop steps) and the `sessions` service (child runs
are the harness's to run).

**Where something goes.** The core if every effect passes through it, if plugins contribute
to it, or if plugins must agree on it ([The core boundary](#the-core-boundary)). An
extension if it needs the host's authority or a native library (a process, a socket, a
database, the loop itself) and the core can stay ignorant of it. An add-on otherwise, and
always if a third party or the agent wrote it.

### In the `corduroy` command

The kernels, providers, MCP servers and secrets of an agent file are extensions with
sections of their own; each section but `[providers.*]` builds a kind of the command's
`Standard` (`<kind>-kernel`, `mcp`, `env-secret`), as `[extensions.*]` does. Any other goes under `[extensions.<name>]`, where `kind` names one of
the kinds the command was built with and the remaining keys are its configuration. Kinds
are compiled in, so a new kind means a command of one's own: the shipped one with more
kinds (`corduroy_cli::command::main_with` and `extension::Extensions`), rebuilt. The agent
file still decides which are loaded. Add-ons are listed under `[[plugins]]`, each with its
grants.

## Without the standard library

The core is to an agent what Rust's `core` and `alloc` are to a program: what every build
has, down to a microcontroller with an allocator and no operating system. What needs an
operating system sits over it, as Rust's `std` does; like `std` it is what most people get by
default, and like `std` a device can do without it and bring its own.

**A direction, not a promise of 1.0.** Nothing 1.0 is [measured against](#what-it-is-for)
needs a device, and no agent has to run on one before the freeze. Building without the
standard library is kept because of what it does to the core: a core that assumes no
operating system stays small, has few constraints of its own, and leaves the most room for
things to grow around it, and one frozen with such an assumption could not lose it later.
So the core and the loop keep building this way, CI keeps checking it, and whatever needs an
operating system goes behind `std` or into an extension. It is where the design steers, not
a veto: when a requirement cannot be met by an extension over the core as it is, the core
changes, and it changes before 1.0. Whether a small, synchronous core forces a design that
an extension cannot work around has a working answer, that it does not
([What the core is made of](#what-the-core-is-made-of)); the extensions
[the freeze waits on](#what-the-freeze-waits-on) are how that is confirmed.

**The core** (`corduroy-core`) is `no_std` and needs only `alloc`. Its `std` feature, on by
default and still free of dependencies, adds the standard library's locks, a call scope per
thread, the system clock, and kernels. Without `std`:

- Its lock comes from the `critical-section` feature, whose implementation the device's HAL
  (or the extension made for the device) supplies, so the core needs no `unsafe` and does not
  choose for the device. A critical section may mask interrupts. The core holds it only for
  its own bookkeeping and while the stream's `Log` stores a commit, never while calling a
  plugin or a listener, so a log for a device stores a commit quickly or hands it on.
- The call scope (`corduroy_core::call`) is one for the program rather than one per thread:
  right for a single-threaded executor.
- There is no system clock. The host gives the stream one (`EventStream::with_clock`); until
  it does, commits are stamped with the epoch, and never go back in time.
- There are no kernels: `Kernel::spawn` and `Process` are processes and pipes.
- The target needs atomic compare-and-swap, which `Arc` does: Cortex-M3 and up, RISC-V with
  the A extension, Xtensa; not Cortex-M0+ (RP2040) or the ESP32-C3. CI builds and lints the
  core for `thumbv7em-none-eabihf`.

**The loop** (`corduroy-harness`) is `no_std` too, with the same `std` and `critical-section`
features, so a microcontroller runs `Harness`: sessions, the journal, operations, gates, tool
calls, child sessions and the model, over the core. It is plain `async` and chooses no
executor. A stop wakes a run waiting for the model through the waker it left, not through a
runtime's notification, and `tests/executor.rs` drives a run, a tool call and a stop with a
hand-written `block_on`. The `tokio` feature (on by default) only moves each tool call to
tokio's blocking pool when the loop runs on a tokio runtime, so a long tool does not hold up
the runtime's other tasks; anywhere else a tool runs where the loop runs, as it must on a
device with one thread. Offering a provider's models to plugins bridges a synchronous call
to an asynchronous provider with a thread and a runtime, so the `provider` plugin does it
only with `tokio`. CI lints the loop for `thumbv7em-none-eabihf` as it does the core.

**The standard extensions** (`corduroy-std`) are what the shipped command carries over the
core and the loop, for an ordinary computer running macOS, Windows or Linux: tokio, the JSON
Lines log, the shell kernel, the HTTP client, the providers, MCP and the WebAssembly host.
`corduroy_std::Standard` is that set as one value whose parts can each be swapped: the
extension kinds an agent file can ask for (a kernel entry is one: `[kernels.<name>]` with
`kind = "shell"` builds the kind `shell-kernel`), the extensions every agent gets (the HTTP
client), how the event log is kept, and whether add-ons are loaded at all. The `corduroy`
command is put together from a `Standard` (`main_standard`), so a command for another kind
of machine changes the parts that do not fit and keeps the rest; one for a device leaves
`corduroy-std` out and brings its own over the same core and loop: a log on flash, a kernel
that is an embedded interpreter, an HTTP client over its network stack. The harness
itself carries none of these: its one optional part is `wasm` (on by default), the core
tier's WebAssembly host, and without it the agent has no add-ons. A build that wants the
shell kernel or the file log depends on `corduroy-shell` or `corduroy-jsonl` (or on
`corduroy-std`), not on a feature of the loop.

**On Windows** the kernel is PowerShell, which every Windows has: `[kernels.<name>]` with
`kind = "powershell"` (`ShellKernel::powershell`), and the default agent's kernel there, with
the system prompt saying so. A cell travels to it as one line of base64, so PowerShell reading
commands from its input never runs half a cell, and is dot-sourced, so what it defines stays;
every stream it writes is its output, and its status is 1 for a terminating error or the exit
code of a native program. Stopping a cell ends the shell's whole tree of processes
(`taskkill /T`), as a process group does elsewhere. CI runs the kernel's tests against
PowerShell 7 on Linux and Windows PowerShell on Windows, where it also builds the command.

*Planned.* The whole test suite on Windows and macOS: CI there builds the command and tests the
core and the PowerShell kernel; the rest of the suite still assumes a POSIX shell.

## The loop

The loop is a part that can be swapped, as the model is. What one step of a run does is a
`Loop` (`src/loops.rs`), registered by name in the `Loops` registry (the `loops` service) and
looked up on every step, so a plugin can contribute one and unloading the plugin takes it
away. `Harness::with_loop` names the loop a harness takes its steps with; the harness
registers `StandardLoop`, the order described under [Goose](#goose-a-loop-of-steps-over-the-persisted-conversation),
as `default`.

The line between the harness and a loop is what must hold for every run, whichever loop takes
it. The harness applies the changes plugins asked for before each step, ends a run asked to
stop, keeps a run that waits for a person or a signal waiting in the log, runs the child
sessions a run waits for, and records how the run ended; `continue_run`, `answer` and
`signal` work the same for any loop. A loop decides what each step does, from the parts
`Run` offers: the session, the standard operations and `operate`, `call_tools` (with each
call's trail in the log), the standard `request` and `ask_model`. Everything those parts do is
recorded, so a run any loop took reads back, resumes and stops the same way. A loop can wrap
the standard one (to watch or adjust its steps) or order the parts differently: ask the model
once without tools, plan before acting, review before answering.

An `Operation` is still how one step is added to the standard loop; a `Loop` is how the order
itself is replaced.

## What an extension can reach

Five things are decided here: what a session is started with and held to, what something
outside a run may do, who takes a run on, what may run at once, and what a step an extension
wrote may read and write. All of it is *planned*. The code is as [The loop](#the-loop) above
and [Durability](#durability) describe it: every session runs alike, and only the host can
raise a signal. [Order of work](#order-of-work) lists what changes.

**A session holds grants of its own, and whatever it starts holds no more.** Today a grant
belongs to an add-on and never to a session: every session is offered every tool, and `read`
reads the whole workspace for whichever session calls it. A session is to be started with
grants in the shape an add-on's have (`fs-read`, `fs-write`, `kernel`, `http`, `llm`,
`tools`, `log-read` and the rest), and they are the whole of what may be done on its behalf:
which files, which hosts, which models, which tools. It is the
[property that leads](#what-it-is-for), applied to a session.

- *Both must allow.* A host call made for a session passes the add-on's grant and the
  session's: a resource has to be in both, as `Scope::within` already says of an add-on a
  plugin loads. An add-on that serves two sessions does for each only what that session may.
- *Narrowing only.* Whatever a session starts holds a part of what it holds itself, and
  never more. The rule is the same whatever is started: a child session, or work handed to
  another harness through a plugin, which acts here only through that plugin and so within
  the session it acts for.
- *What is not narrowed is held as the starter holds it.* A child started with nothing said
  is as capable as its starter, as `delegate`'s is today. The session a person starts holds
  what the agent file allows, with nothing narrowed.
- *The tools offered are the session's `tools` grant.* The model is offered, and may call,
  only the tools that scope reaches. The registry stays one flat set, and a tool is in a
  session by its name.
- *The record is among what a session sees.* `log-read` is narrowed like the rest, so one
  agent of a pair does not read the other's session.
- *A tool from an extension or an MCP server is offered or not.* It is code the grants do
  not reach into, so a session's file grants do not hold it, and offering one to a narrowed
  session is the decision about what that session reaches through it. A server's tools can
  be offered in part, by name: those that read and not those that write.
- *A session that needs more asks.* What it was refused is recorded, as a refusal to an
  add-on is, and it asks the ordinary way: a gate's ask, a question to the person, a message
  to whoever started it. Only the person's answer lifts a ceiling.
- *The grants ride in the call scope.* The core's call scope says which session a thread
  serves. It is to say what that session is granted as well, so the WebAssembly host and a
  kernel read the grants where they already read the session. That is in the core, and is
  marked before the freeze. Work handed to another thread has to take the scope with it, or
  it is done under no session's limits at all (B17 in [Open questions](open-questions.md)).
- *It is the fourth guarantee, one step further.* The
  [six](#what-is-safe-by-default) gain no seventh: the fourth is read as covering a session
  started as well as an add-on loaded, and its test covers both.

**A kernel serves sessions.** What a session may read has to hold for `cat` as it does for
`read`, so a kernel keeps the sessions it serves apart and holds each to its grants.

- *Nothing new among the core's kinds.* A kernel is called on the chain of a tool call, so
  it reads the session, and with the above its grants, from the call scope.
- *State for each session.* A shell kernel keeps a shell for each session that uses it,
  started on first use. A child does not inherit its starter's variables or directory, a
  sub-agent no longer moves its parent's working directory, and one session's cell does not
  wait behind another's.
- *Confined to both.* A session's shell reaches what the kernel was
  [given](#what-is-safe-by-default) and the session is granted, a resource being in both.
  The operating system fixes a process's confinement when the process starts, which is why
  there is a shell for each session and not one shell whose limits change with its caller.
- *Why not a kernel for each role.* A kernel is a line the person writes in an agent file.
  If a narrower shell needed a kernel of its own, the agent could never build a narrower
  role itself, and [the second use](#what-it-is-for) is the agent doing exactly that.
- *No silent downgrade.* A kernel says whether it can hold a session to its grants: a method
  on `Kernel` with a default that says no, marked before the freeze. One that cannot (the
  plain shell, and for now any shell on Windows) is refused to a narrowed session.
- *The registry stops locking a whole kernel.* `Kernels` holds each kernel behind one lock
  today, so every cell waits for the one before it. That shape freezes with the core, and
  changes first (B5).
- *What a kernel starts for no session is held by the kernel alone.* An MCP server started
  through `Kernel::spawn` serves every session, with what the kernel was given.

Whether "everything but this directory" can be said to the operating system is for the
[confined kernel](#what-the-freeze-waits-on) to find out. Landlock, for one, grants a
directory with what is under it, and has no rule that refuses a directory inside one it
allowed.

**What a session is started with.** Five things, written into its `session/start`, so the
log says what each session ran as:

| | What it is |
| --- | --- |
| Model | `<provider>/<model>`, one the starter's `llm` grant reaches. Kept for the session's life, [as decided](#what-the-core-is-made-of). |
| Loop | A name in `Loops`. |
| Instructions | The text its system prompt starts with. Plugins' prompt parts follow it, rendered for the session and so under its grants. |
| Grants | As above. |
| Limits | A set that can grow. Turns are its first entry, counted as the session's loop counts them. A budget of time, tokens or cost has a place to go (G20). |

Gates are not on the list. They hold for every session, since taking a guard away widens
what an agent may do. A gate is told the session, and may treat sessions differently.

- *The values are recorded, not a name for them.* A session picked up after its agent file
  changed runs as it started. A `session/start` that says none of this, as every log written
  so far does, runs as the agent is configured.
- *Fixed for life.* Nothing a session is started with changes while it lives. The grants are
  a ceiling that only the person's answer lifts (a host allowed, a credential masked), and
  the answer is recorded. A starter that wants a wider child starts another one.
- *Who says.* The person, for a session they start: the agent file and the command line.
  Whoever starts a child, for the child, within its own session.
- *Instructions replace.* A child's instructions take the place of the agent's own: a test
  writer should not inherit a prompt that describes the code. A starter that wants the
  agent's own text as well names it with a marker the harness fills in, so an add-on never
  has to read that text to pass it on.
- *Coming back to a session.* It runs as it started. `--model` or `--provider` naming
  something else for a session that exists is an error, which says what the session runs on
  and that a new session is how to choose again.
- *Starting a session is a decision point.* The loop declares `session/start`, and the gates
  are asked with who is starting it (the person, a session's agent through a tool, a
  plugin), its parent, and what it would be started with. That a reviewer does not run on
  its starter's model, or that an orchestrator does not start orchestrators, is then a
  gate's to say. A session a gate asked about is recorded and waits, in the log, for the
  person's answer.
- *What a grant of `sessions` means.* An add-on granted the `sessions` service can start an
  agent as capable as the session it serves, whatever the add-on's own grants are. That is
  accepted, and stated: it is what `delegate` is for. The gate and the limit on depth are
  what bound it.
- *One file is one agent.* A role is written by whoever starts the session, because the
  agent has to be able to invent one. A role the person wants fixed lives in the
  configuration of the add-on that starts it, as `review`'s configuration names its model
  today. Named agents in an agent file can come after 1.0 as keys of their own, breaking
  nothing. A second agent file is a second harness, which stays possible
  [as a plugin](#what-it-is-for).

**What something outside a run may do.** A connected harness says something, a webhook
fires, a timer goes off. None of it happens inside a tool call, and today only the host,
which holds the `Harness`, can act on it. The `sessions` service, which today starts a child
from inside a tool call, gains three things that work from anywhere:

| | What it does |
| --- | --- |
| Raise a signal | Brings a waiting tool call its result. A signal is named `<plugin>/<what>`, and a plugin raises only those under its own name; the harness's own, such as a child's end, go under a name no plugin can hold. A signal that arrives before its wait was recorded is kept, and answers the wait when it comes. Today it would be dropped, and a quick reply lost. |
| Start a session | With a first message, and what a session is started with. It has no parent, and its `session/start` records who started it. It passes the gate like any other. |
| Send a session a message | To a session with no run under way, it starts one. To one that is working or waiting, it is recorded at once and starts the next run when this one ends. Either way the message is recorded with who sent it, so a review tells it from the person. |

- *The same holds for the person.* A message typed while a run is under way waits in the log
  for the run to end. A run is still what lies between its first message and its `turn/end`.
- *Steering is not promised.* Slipping a message in before the model's next request is
  something an extension can build as a step of the loop. What the harness promises is the
  queue.
- *Add-ons too.* It is one service, reached through the `service` grant as `delegate`
  reaches it today. An extension reaches the same three, and reading (which sessions there
  are, how each stands, which calls wait for its signals), through a handle it lists in
  `inject`. The ceilings differ. An extension is trusted, so a session it starts may hold
  what the agent file allows. Outside any session, a session an add-on starts holds at most
  the add-on's own grants. An automation the agent wrote can then start a run when an event
  fires, without holding more than it was given.
- *A stop reaches a plugin whose call has returned as an event.* A run that waits for a
  plugin's signal and is stopped is ended here, and the plugin hears of it as it hears of
  anything else (`run/stop`), and tells the other side.
- *A scheduler is then an extension,* and is not in 1.0.

**The host takes the run on.** A call from outside only records: the signal, the message or
the session's start goes into the log, and the call returns. The harness has one driver,
which the host runs, and it takes on every session the log says is ready. No plugin runs a
loop.

- *Why not the extension's own thread.* The thread a reply arrives on would have to run the
  agent's loop until the run ended or waited again. Every such extension would need an
  executor and the whole harness in its hands; there would be as many drivers as extensions
  and nobody deciding the order, or how many sessions run at once; a stop and the display
  would have to find runs on other people's threads; and an add-on, which is called and
  returns, could never do it. With one driver the same call works from an extension, from
  an add-on, and on a device with one thread.
- *It is an operating system's split.* The host is the kernel: plugins ask it for things,
  and are told when something arrives.
- *Nothing is lost between recording and running.* What was recorded and not yet run is
  taken on by the next process, under the rules a [resume](#durability) already follows.
- *A wait names its plugin.* Since a signal is `<plugin>/<what>`, a run waiting for a plugin
  that is not loaded can be said to be, where today it would wait in silence.
- *A host that drives by hand still can.* `Harness::run_in`, `answer` and `signal` stay for a
  program that embeds the harness and awaits its runs itself.

What the command does follows from it:

| | Takes on | When its run waits |
| --- | --- | --- |
| `corduroy` (talk to the agent) | Whatever arrives, for any session. Work for another session runs in the background, with a notice. | Stays, as a conversation does. |
| `corduroy run <prompt>` | Its own session, and what that session waits for. | For a signal, it stays until the signal comes: leaving would end the process, and with it the connection the reply would come on. For a person's answer with nobody there, it exits and the run waits in the log, as today. |
| A mode that stays up | Whatever arrives, with no terminal and no protocol of its own. It is what the other side of a [connected harness](#what-the-freeze-waits-on) runs as. | Stays. |

- *Ctrl-C.* It stops a run that is working, as today. While a run only waits, it ends the
  command and the run stays waiting in the log; `corduroy stop` is what ends the run.
- *The notice is the front end's.* A line in a terminal, a toast in a desktop application.
- *A session from outside can be held for the person.* It passes the gate at
  `session/start`, so a policy can ask before it goes on in the background.
- *A protocol for front ends is still open* (H11). The mode that stays up serves plugins,
  not other programs.

**The session is the unit of concurrency.** The driver makes the question real: the
person's run, a session started from outside and several children can all be ready
together.

| | Rule |
| --- | --- |
| Inside a session | One thing at a time: one run, one step, one tool call, in order. |
| Across sessions | The driver may run them side by side. How many is a setting. One is allowed, and is what a device with one thread gets. Nothing may depend on two sessions overlapping. |
| What is shared | Takes one caller at a time. The log puts commits in one order, and plugins are loaded and unloaded between steps. |

- *Fan-out is several children.* Three `delegate` calls in one message start three sessions.
  The driver runs them together, and the parent goes on when each has answered. Children
  are no longer run one after another. How deep they nest is still a limit (G16).
- *No parallel tool calls in the loop at 1.0.* A tool that runs a set of calls together, and
  counts as one call of its session, is something an extension may offer. It has to be an
  extension, since an add-on makes one host call at a time, and calls to one add-on still
  take turns within a session. So that a form in the loop can come later, the record's
  contract says now that each call's own trail is in order (`tool/call`, `tool/start`,
  `tool/result`) and the trails of different calls may interleave.
- *An add-on is an instance for each session.* Today one instance serves every session, one
  call at a time, so a build running in `bash` for one session holds `read` and `edit` for
  every other. The host is to keep an instance for each session that calls an add-on, and
  one for what belongs to no session. An add-on's memory is then a session's, and is never
  shared: what must be shared or kept goes through `state`. It costs memory, little for a
  Rust add-on and 9 to 13 MB an instance for JavaScript and Python. It is said now because
  it cannot be said later: an add-on that came to count on one memory for the whole agent
  would break.
- *Each session has its own shell,* as above.
- *A device runs one session at a time.* [Without the standard
  library](#without-the-standard-library) the call scope is one for the whole program, and
  that is the contract there: the driver takes one session, then the next.
- *Two sessions and one file.* The harness sees every write an add-on makes and none that a
  shell command makes, so it does not promise that two sessions never write the same file.
  It does two things. Grants keep agents apart: a directory each. And the shipped `write`
  and `edit` refuse a file that changed since the session last read it, with an error that
  says so. That catches another session, a command and the person's own editor alike,
  without knowing which it was; where a tool made the earlier write, the log shows it.
- *In which order listeners hear the commits of sessions that run together* is still open
  (C3).

**A step reads everything, and writes through effects.** The rules ask whatever runs a tool
to leave the call's trail through the journal, and the trail's methods are private to the
harness. Meanwhile `Journal::record` and `apply` are public, so a loop can write any message
into a conversation with no trail at all. The read side is closed and the write side open,
and it is to be the other way round.

- *Read everything.* A session's snapshot shows each message with its event's id; the
  conversation as the model is sent it; for each call, what the gates said and how far it
  got (not started, running, finished with its result, waiting for a signal); what a
  compaction could summarise; how the last run ended; and what the session was started
  with. With that an extension can write a compaction strategy, as a step that returns
  `Effect::Compacted`.
- *Write through effects and the run's parts.* An operation returns effects. A loop uses
  what `Run` offers, with one part added: run a single tool call, and leave its whole trail.
  Nobody outside the harness writes half a trail, or a message the journal did not make.
- *Trusted is not leave to be careless.* An extension holds the host's authority, and the
  write side is closed to it all the same. That a tool is
  [never run twice](#what-the-record-promises) unless it said that is safe is promised of
  every run, and one extension writing a careless trail would break it for all of them.
- *An operation reads and returns; a loop acts.* `Operation::run` is given a context in
  place of the bare session: the snapshot, what the session was started with, whether the
  run is stopping, and the registries. The context can grow. An operation still only says
  what to record. A step that calls a tool or asks the model is a loop, which can wrap the
  standard one; and a tool that needs an agent's work starts a session.
- *A loop is not handed the whole harness.* `Run::harness` gives a loop `run_in` on any
  session, which goes around the driver. A loop is to get the registries and the settings,
  and to reach other sessions as everything else does: start, signal, message, list, read.
- *The journal is the host's.* A program that embeds the harness and drives it by hand
  keeps it. Loops and operations do not write on it.

**What is not promised.**

- *That a session's limits reach inside a tool from an extension or an MCP server.* Offering
  the tool is the decision.
- *That two sessions never write one file,* least of all from a shell. A lock on files is
  not part of 1.0.
- *That sessions do run at once,* or in which order the driver takes them.
- *Steering a run under way,* named agents in an agent file, a scheduler, and a protocol
  for front ends.

## What is stable

Contracts hold at one of two levels, so that extensions and add-ons written against them
keep working.

- **Frozen**: the primitives are closed. After 1.0 a frozen contract gains no new concept:
  no new registry, no new trait, no new kind of thing a plugin contributes. It gets bug
  fixes, and it grows only at places marked for growth before 1.0 (a `#[non_exhaustive]`
  type gaining a field, a trait gaining a method with a default, a message gaining a kind
  of part). What is not marked is closed for good. The marked places are few on purpose,
  since each is an exception to "read it once and it stays true", and the set of primitives
  has to be complete before 1.0 because it cannot be added to after. For the core that set
  is decided: [eight kinds](#what-the-core-is-made-of) of thing a plugin contributes.
- **Stable**: changes only by adding. New things arrive, and nothing written against what
  was there breaks.

The idea being worked to is that what is frozen is what things are built from, and what
makes building easier sits on top, in extensions, and is stable. It is the current aim, not
a settled rule: where the line falls may move as the proofs below are built.

| Contract | Where | At 1.0 | Rule for change |
| --- | --- | --- | --- |
| The core | `corduroy-core`: the plugin tree, capabilities, the event stream, the registries and their traits | frozen | New trait methods come with defaults (as `Kernel::spawn` did). Types that grow are `#[non_exhaustive]` with constructors (`ToolSpec`, tool results), so a new field breaks no extension. Both only where growth was marked before 1.0. What the core offers without `std` only grows: something that needs an operating system is added behind `std`. |
| The plugin world: exports and shared types | `wit/corduroy.wit` | frozen | The exports are closed at six. Changing an export, a type a guest sees, or a function that exists is a new major version of the package, and guests are rebuilt against it; so they change only together, rarely, and never to add what an import or a service could add. See [How the plugin world grows](#how-the-plugin-world-grows). |
| The plugin world: imports (the capabilities) | `wit/corduroy.wit` | stable | A minor version adds an import interface, or a function to one where its grant already covers what the function does. Neither breaks a built add-on: one that does not import the addition is unaffected, and a host without it refuses, at load, an add-on that does. So the set of capabilities grows after 1.0, and an add-on built against 1.0 loads on every 1.x host. |
| The loop's traits and registries | `corduroy-harness`: what extensions implement or register in (`Operation`, `Operations`, `Loop`, `Loops`, `Provider`, `Providers`) | stable | As for the core: defaults for new methods, `#[non_exhaustive]` for types that grow. |
| The WebAssembly host's Rust API | `corduroy-wasm` | stable | Added to, not changed. |
| The log | `corduroy-jsonl`, the topic table in `src/session.rs` | stable | The header's schema version changes when a line's shape does. Events gain fields; they do not lose or repurpose them. A log written by 1.0 or later is read by every later version. The other way round is not promised: an older program is helped with a newer log only by the rule that readers ignore topics and fields they do not know. |
| The standard extensions and the agent file | `corduroy-std`, `crates/corduroy-cli` | stable | What a kind does and how it is configured stays; kinds and keys are added, not renamed. An unknown key is an error, so a typo is never a silently ignored grant. |

Everything else may move. That includes the shipped add-ons: their tools and configuration
keep changing after 1.0, as what the agent builds does.

**Before 1.0 nothing is frozen.** The levels are where each contract is heading, and
everything may still change on the way. The core is meant to get there first, possibly
ahead of 1.0. The log in particular starts from a clean baseline: shapes that only
unreleased versions wrote (the first schema, without times, and messages in oi-kit's shape)
are read today and need not be read by 1.0.

### What the freeze waits on

Whether the extension tier is enough cannot be argued, only tried. Four things are built
before the freeze, each against the surface as it then stands and without changing the
core. What one of them cannot do becomes a change to the core, made before 1.0, or an
entry in [Open questions](open-questions.md). There is no 1.0 until each has passed or its
failure has been dealt with.

| Proof | What it shows |
| --- | --- |
| A sandboxed kernel | That what the agent starts (an MCP server it wrote included) can be confined through the kernel trait alone. What comes out of it ships, as the default agent's shell ([What is safe by default](#what-is-safe-by-default)). |
| Another harness connected as a plugin | That a long-lived connection exchanging messages fits over a core whose every trait is [called and waited for](#what-the-core-is-made-of). The hardest test of whether the small core forces a design. It has to show four things: a task handed over and its result received through a wait that survives a restart; the other side's progress seen here as events; a stop here reaching the other side; and a message the other side sends unasked getting in. |
| A second store for the log | That the `Log` trait is enough for a store that is not the file: a folder with a file per session, read on request ([What the record promises](#what-the-record-promises)). |
| The test-driven pair, end to end | The second [measured use](#what-it-is-for): two agents with different loops, tools and views finishing one task. It needs an add-on to define a loop and sessions to differ. |

Not waited on: a scheduler and kernel snapshots, which serve unattended work and come after
1.0; secrets from a keychain and code mode, which may be built before it and need not be.

The `message` event's payload is the core's `Message` (`{"role", "parts"}`, see
`corduroy_core::message`), part of the log contract like the rest: parts gain kinds, and a
reader keeps what it knows and skips the rest. Logs written while the payload was oi-kit's
message are still read today, so they resume.

Two rules came with the capabilities that reach outside:

- **Secrets never enter the log.** A secret is handed to a plugin as an opaque handle (the
  `credential` resource), and host functions that take one (an HTTP header) write its value
  into what they send and record its name, not its value. Response bodies are not recorded
  either, since they may echo what was sent. HTTP clients must not follow redirects: the
  host checked one host against the grant, and a redirect elsewhere would go around it.
  For the same reason the host refuses URLs a parser could read differently from its own
  check: a backslash, whitespace, a control character, or a user name before the host.
- **Time and randomness are host calls.** A WebAssembly plugin has no clock of its own, so
  given the same host-call results it computes the same thing. That keeps
  [re-executing a plugin](#what-the-record-promises) possible, though it is not promised,
  and WASI stays out of the linker to keep it true. The guest-language spike confirmed it for JavaScript and Python, with a
  catch: built without WASI their clocks trap or stand still and their random numbers repeat,
  so `clock`, `random` and `log` are needed as soon as either language is.

## How the plugin world grows

Four things about the plugin world after 1.0 are decided here: what is promised to an add-on
that is already built, how the capabilities grow, that the exports do not, and which
versions a host accepts. All of it is *planned*. Today the host drops the version when it
reads a component's imports, so an add-on built for another world loads and then fails when
it is instantiated, with the engine's own error; [Order of work](#order-of-work) lists what
changes.

**A built add-on loads on every 1.x host.** The promise is about the built thing, the
`.wasm` file, and not only its source. An add-on built against the world of 1.0 loads on
every later host whose world is 1.x, with nothing rebuilt, and each function it calls does
what it was documented to do. What the agent built yesterday is then still there after the
command is upgraded, which "the agent extends itself" needs and a rebuild for each release
would quietly break. A change that would stop a built add-on loading is a new major version
of the world.

- *Scripts are inside the promise.* An automation the agent writes is mostly not a `.wasm`
  file of its own: it is a script the script host runs, and the script host ships with the
  command. What such a script depends on is the `host` object it is handed. `host` is not a
  capability and grants nothing. It is how a script reaches the capabilities (`host.fsRead`,
  `host.llm`, `host.state`), each call still checked against that automation's own grants.
  Its names and shapes hold for all of 1.x, as the world they mirror does, and so does the
  shape of the plugin object a script returns. A script written under 1.0 runs on every 1.x.
- *Shipped add-ons need no promise.* They are rebuilt with every release.
- *What "loads" covers.* That the add-on loads, and that each function does what its
  documentation says. Not how fast it is, how fuel is counted, or what the default limits
  are.
- *It is kept by a test.* A built add-on from each released world is kept as a fixture and
  loaded by every later host. An upgrade of the WebAssembly engine that cannot load one
  waits until it can.
- *One exception stands.* A flaw that breaks one of the six guarantees may be fixed by a
  change that breaks a frozen contract ([What is safe by default](#what-is-safe-by-default)),
  this one included.

**Backwards compatible, forwards adaptable.** The promise runs one way. An older add-on on a
newer host always loads. A newer add-on on an older host is not promised: it loads if
everything it asks of the host is there, and is refused otherwise. Growth starts from a
small set and builds on it, so what a host was not built to handle is handled by the release
that adds it and by none before. What an older host does with something it does not know
depends on what is being asked of it:

| What the host does not know | What it does |
| --- | --- |
| Something the add-on needs from it: an interface, or a function of one | Refuses the add-on at load, naming what is missing. It cannot be ignored, since the add-on would call what is not there. |
| Something in data it is handed: a field, a topic | Ignores it, as a reader of the log does ([What is stable](#what-is-stable)). |

**An interface gains a function only inside its grant.** An import interface is a
capability, and a capability is what a person grants. The engine would let an interface gain
any function: an add-on built for 1.0 imports the functions it calls, and links against a
host whose interface has more. The limit is the grant. A function added to an interface
reaches every add-on already granted it, and the person is not asked again, so it may do
only what that grant already covered.

| What is added | Where it goes |
| --- | --- |
| Another way to the same thing: the same resource, under the same scope, with the same kind of effect. A different form of result, or more to say about what is asked for. | A function of the interface, in a minor version. Reading a file as bytes is `fs-read`'s: the same files, and still only reading. Listing models is `llm`'s: it shows only the models the grant reaches. |
| Anything that does something new | A new interface, with a grant of its own. A running tool reporting how far it has got is not `session`'s, which only answers questions about the call being served. A tool call that waits for a signal changes how a run proceeds. |

The test is whether the person who approved the grant under 1.0 would be surprised by what
it now allows.

- *What freezes is what a grant means, not its list of functions.* Each capability has one
  sentence saying what holding it covers: the table under [Capabilities](#capabilities), and
  the interface's own text in the WIT. Before 1.0 each sentence is read again, so that it is
  as wide as is meant and no wider. After 1.0 it does not change, and it is what a proposed
  function is held to.
- *A function, once there, does not change.* The engine checks types, so a changed
  signature would stop a built add-on loading. A different input or result is a new
  function beside the old one. The same holds for the types add-ons share with the host: a
  record gains no field and a variant no case within 1.x, so what must grow is marked
  before 1.0 or is JSON text (E1, E2 and E13 in [Open questions](open-questions.md)).
- *The cases already known go in before 1.0.* Progress from a running tool and a tool call
  that waits for a signal are interfaces of the 1.0 world, so that the set is as complete
  as it can be made when it is fixed. Their shapes are still open (E14, E6). Reading bytes
  can come at any time, before 1.0 or after.
- *A new ability is tried as a service first.* An interface added after 1.0 is there for
  good. So an ability nobody has used yet is offered by an extension as a service, reached
  through the `service` capability
  ([the pressure valve](#three-tiers-core-extensions-add-ons)), and becomes a typed
  interface in a minor version once it has proved itself.

**The exports are closed at six.** What the host calls in an add-on is `activate`,
`call-tool`, `on-event`, `prompt-part`, `gate` and `handle-call`, for all of 1.x. Whatever
the host newly needs to ask an add-on goes through `handle-call`, under a service name the
harness defines, as the loop already asks whichever add-on provides `compaction`. It
follows from the kinds being [closed at eight](#what-the-core-is-made-of): a new kind is a
service, and a service needs no export.

- *The cost is types.* What is asked through a service is JSON text, so a wrong shape shows
  when the call is made and not when the add-on is built. That is accepted.
- *A seventh export is for 2.0.* Something asked through a service that proves itself may
  become a typed export in the next major version, which is a breaking change. Anything
  already known to need an export has to be found before 1.0.
- *Exports stay mandatory.* Every add-on has all six, and the SDKs fill in the ones a
  plugin does not write. A host that tolerates a missing export could come later and would
  break nobody, so it is not decided now.
- *The harness's service names are reserved.* If a later release asked add-ons for a
  service under a name an add-on already used for something of its own, that add-on would
  get calls it does not understand. A prefix is set aside before 1.0 for the services the
  harness defines. Which prefix, and whether `compaction` and `sessions` move under it, is
  still open (E18).
- *A loop from an add-on is not an export.* How an add-on says what a run's steps are is
  still open (E12), but it is a service the harness asks or a description it reads, not a
  seventh way of being called. A model served by an add-on, if there ever is one, comes the
  same way.

**Which versions a host accepts.** The world's version is the WIT package's, and it reads as
a semantic version does:

| Change | Version |
| --- | --- |
| A new import interface; a function added to one, inside its grant | minor |
| Wording that changes no shape | patch |
| Anything else: an export, a shared type, a function that exists, something removed | major |

The host checks when it loads an add-on, before anything is instantiated, and says what it
found in its own words:

| Add-on built for | On a 1.x host |
| --- | --- |
| An earlier 1.x | Loads, always. |
| A later 1.x | Loads if every interface and function it imports is there. Otherwise it is refused, with a message naming the version it was built for, the host's, and what is missing. |
| Another major version | Refused, naming both versions. |
| Any version, before 1.0 | The host promises to load its own version only. |

- *The check is on what the add-on imports, not on the number.* An add-on built against 1.3
  that uses only what 1.0 had loads on a 1.1 host. A Rust add-on imports only the functions
  it calls. A JavaScript or Python one imports every function of each interface in the
  world it was built against ([Guest languages](guest-languages.md)), so it more often
  needs a host as new as the one it was built for. Either way this is the forwards
  direction, which is not promised.
- *The engine already matches versions this way.* wasmtime's linker resolves an import of
  `@1.0` against a definition of `@1.2`, and the other way round (checked in the pinned
  51.0.0). The host's own check is there for the message, and so that the rule is ours and
  not a property of one engine.
- *Nothing is promised across a major version, and nothing is ruled out.* A 1.x host
  accepts 1.x. Whether a 2.x host also loads add-ons built for 1.x, by carrying the old
  world beside its own (the engine can link both, and the host would then implement and
  keep safe two worlds), is decided when 2.0 is designed.

## Events, gates and hooks

What other harnesses call hooks are two different things here, and both are plugins.

- **Reacting is an event.** Events stay fire-and-forget: the emitter does not know who
  listens and gets nothing back. A hook that observes (log this, count that, nudge the
  prompt) is a plugin with a subscription. This keeps the log a plain record that reads
  back without re-running anyone's decision.
- **Deciding is a gate.** Where the loop needs an answer before it proceeds, it asks the
  gates registered for that decision point, in a fixed order, and the strictest answer wins
  (deny over ask over allow). The outcome does not depend on which plugin loaded first. The
  decision itself is then recorded as an event.

Decision points are *declared*. The core declares the two it asks at itself
(`corduroy_core::POINTS`): `tool/call` and `plugin/load`. An extension that needs an answer
before something of its own happens declares a point (`Gates::declare`, an effect like any
other, with a description of the request it sends) and asks the gates there; the user's
input and the request about to be sent to the model are the likely next ones. A point
nobody declared does not exist: asking at one is denied, and a WebAssembly plugin that
lists one is refused at activation, so a policy never silently guards a point that is never
asked. *Changed from the plan:* the points were to be a closed list in the core, each added
as a plugin needed it, which made every new point a change to the core. A plugin that wants
to ask other plugins a question of its own, rather than a yes or no, does so through a
[service](#plugins-extending-plugins), not through the event stream.

A deny or an ask always carries a reason. A denied tool call becomes an error tool result
holding that reason, so the model learns what was wrong and can try again, as with any other
tool failure.

**How `tool/call` is gated (built).** A loop step, `GateOperation`, sits between the prompt
and the tools step. For each call of the model's message it asks the gates once, with
`{"session", "id", "tool", "input", "read-only", "caller"}` (`caller` is `agent`), and
stores the decision as `gate/decision` in that step's commit, so the decision is in the log before the tool can start. An allowed
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

## What the record promises

The log is for three things, in this order: **recovering** a run, **reviewing** what an
agent did, and **the agent reading its own history** to find what to build next. Review is
part of the purpose and not a convenience: a person can let an agent extend itself because
what it did, what it was refused and where it failed can be checked afterwards, failures
included. What follows is decided; which parts are built is said for each.

**Everything that can be logged is logged.** That is the working assumption every other
answer here is read against. What should be kept *out* (a private value in a prompt or a
tool call) is not decided, and stays in [Open questions](open-questions.md) (C6).

**How it is read.** By session, filtered, in order: the tool calls, the asks and their
answers, where a run stopped. A reader wants the shape of a run first (search, reproduce,
fix) and the single steps under it on request. That outline is a view a reader builds from
the events and their causes; the log does not store one. The agent reads the same events
(`log-read`), across sessions, to see where runs keep failing and what it should automate.

**Reading back and resuming are promised; re-executing is not.** "Replay" has meant three
things, and they are kept apart:

| | What it is | At 1.0 |
| --- | --- | --- |
| Reading back | Walking a session's events in order to see what happened | Promised. |
| Resuming | Continuing an unfinished run from the log ([Durability](#durability)) | Promised, as a guarantee: a tool is never run twice unless it declared that safe (`replay_safe`), and the only call ever in doubt is the one that was running when the process died. |
| Re-executing | Feeding the log back through code to rebuild state or reproduce a run | Not promised. Nothing rules it out later: time and randomness stay host calls so that it remains possible. |

What does not come back on a resume is stated, not hidden: a kernel's processes and
variables are gone and the model is told so, and an add-on's memory is gone but for what it
wrote through `state`. The model rebuilds what it needs. Resuming is expected to work on
the same machine with the same build. On another machine or a later build the log is still
read ([What is stable](#what-is-stable)), and resuming there is best effort. So that a
reader or a resume can tell which it is, each process that opens a log records what it is:
the program's version, the plugin world's version and the platform (built: `process/start`,
the first thing the `corduroy` command writes; the identity of the machine is left out until
someone needs it, since it is also something to keep private).

**Live output is a preview; checkpoints are the record of it.** Text from a model as it
arrives, and output from a tool while it runs, reach the terminal as they come. It is
expected of a harness and is part of 1.0, and it is not the core's (*planned*):

- *A live channel in the loop.* Fragments go to whoever displays them, at whatever grain
  the provider or the tool gives, and are not events. The core's rule keeps no exception:
  every event is in the log.
- *Checkpoints in the log.* While a reply or a long tool call is under way, the loop
  commits what has arrived since the last checkpoint as an ordinary event, at a coarse
  grain, and always when the run is stopped or fails. A crash loses at most what came after
  the last checkpoint, and the log shows how far the reply or the command got.
- *The whole is still committed.* The finished message or result is one event, as now.
  Nobody reconstructs anything from checkpoints: a reader, the model and other agents use
  the whole one, and checkpoints matter only where no whole one exists. A view of a session
  may hide them once it does. Nothing is overwritten.
- *Other agents get finished messages.* A plugin or a connected harness subscribes to
  events as today and is not in the path of every fragment.

How often to checkpoint is a setting: off, every fragment, or an interval in between, a
few seconds by default. Streamed text is then stored at most about twice, and the number of
events is bounded by time and not by tokens. A reply cut off part-way is not continued from
its checkpoint, because providers differ on whether they can: on resume the partial text is
evidence for the reader and the request is made again. This needs a streaming method with
a default on `Provider`, a way for a running tool to report output through the call scope
(the one piece in the core, to be marked before the freeze) and topics for the checkpoints.
The split between channel and checkpoints is the decision; the mechanism is confirmed
against a real provider before it is fixed.

**Whole sessions may leave the log, and nothing smaller.** The stream never removes or
rewrites an event: while a program runs, the log only grows. A store may remove a whole
session, with the sessions it started, as maintenance the person asks for through the
command (built: `corduroy sessions remove`; `corduroy_harness::removal` says which sessions
go and what the markers are, so every store removes the same):

- Ids are never reused. A reader tolerates gaps in them, a cause that names an event no
  longer there, and an event or a parent that names a session the log does not know.
- A removal made through the program leaves a marker: which session, and when
  (`session/removed`, one for each session, stored with the ids after every event the log
  held). The record then says that something is gone, though not what it was.
- Editing the files by hand is outside the contract. What it leaves behind is covered by
  the same tolerance, with no marker.
- Only a person removes at 1.0. An agent removing history would be a capability of its own,
  apart from reading the log, never granted by default and always leaving the marker; the
  set of capabilities grows after 1.0, so it can wait.
- Nothing expires by itself at 1.0. Keeping history for a limited time, or a nudge when it
  grows large, is policy that can be built on removal later.

**A read is complete; memory is not promised.** A read promises a complete answer about
what the log holds, not that the stream holds it in memory. Three things are marked in the
core before the freeze for that, and built:

- The `Log` trait has a read side with a default (`Log::read`), so a store can be asked for
  what the stream does not hold. A store that hands the stream every event when it is
  opened leaves the default, which finds nothing. One that hands over part answers for the
  rest, and says where numbering carries on (`EventStream::continuing`), since the last
  event stored may be one it kept back.
- Reads that are paged and may fail (`EventStream::read`, and `read_where` with a filter
  applied before the limit) are the route that reaches the store: they merge what the
  stream holds with what the store returns. The harness rebuilds sessions and the list of
  them through it, the WebAssembly host rebuilds an add-on's `state` and answers
  `log-read` through it, and `corduroy log` prints through it.
- The existing ones (`events()`, `session()`, `matching()`, `after()`, `get()`) stay as the
  convenient form. They cannot fail, so they answer from what the stream holds: the whole
  log while the store loads everything, which at 1.0 it still may (the JSON Lines file
  does). What they promise once a store keeps part of the log back, and which part it
  hands over, is still open (C21).

**The second store is a folder.** The [second store the freeze waits on](#what-the-freeze-waits-on)
is this one (*planned*). A workspace is a folder holding one workspace file and one file per
session. To the stream it is one log with one order of ids; the store decides where each
event goes:

| File | Holds |
| --- | --- |
| The workspace file | Events that belong to no session (plugins loaded and unloaded, what add-ons keep through `state`), each session's start with its parent, and the markers of removed sessions. It is also the list of sessions. |
| A session's file | That session's events. |

Opening a workspace reads the workspace file and the sessions in use, so starting up does
not grow with history; an old session is read when asked for, removing one deletes a file,
and searching history is searching files. A commit that touches two files (a session
starting) must still be stored whole or not at all, which is the store's to solve and part
of what the proof shows. Whether it replaces the single file as the default is decided once
it works; the single file stays as the simple store either way. Anything faster than
paging through old sessions (an index, search) is an add-on's or an extension's to build
beside the log, not a promise of the log. Branching a session from an earlier point fits
the layout as a new file whose start names the event it branches from, and is still an
open question (C14).

## Durability

A step of the loop reads the persisted conversation and returns effects for the journal to
persist. One step is therefore one commit.
All but the last point are built; kernel state is *planned*.

- **Commit, then deliver.** `Journal::apply` turns a step's effects into one batch
  and hands it to `EventStream::commit`. The batch is written to the `Log`; only then is it
  added to the stream, are listeners called and is the next step allowed to start. A refused
  write closes the stream, since the log may hold part of the batch, and is a broken
  pipeline: `Harness::run` returns `HarnessError::Log`. Emitters with nobody to report to
  (plugin lifecycle, events a plugin queued) drop the error, because the next commit of the
  run fails with the same one.
- **Stores are plugins of the log.** The core carries the `Log` trait and nothing that
  touches a disk. `corduroy-jsonl` is the first store: a header line with the schema
  version, then one line per commit (each event with its time), written with one `write`
  and synced. Between runs it can [remove whole sessions](#what-the-record-promises),
  writing the file again beside the old one and putting it in place in one step. An
  unfinished last
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
  to the model, so the conversation keeps one result message per step (`PluginTools`).
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

The built-in registries the loop itself consumes (`tools`, `prompts`, `gates`, `providers`)
and those host functions consume (`kernels`, `models`, `secrets`, `http`) are the cases where
the harness is the consumer. A plugin that wants its own extension point (an MCP plugin accepting
server definitions, a skills plugin accepting skill sources) provides a service.

## Automations

The purpose of the plugin system is that the agent can adapt the harness to its task: having
done something by hand, it writes an automation that does it from then on without a model
in the loop. This extends Hermes's idea from skills (text the model reads) to plugins (code
the harness runs). An automation is a WebAssembly plugin with three parts:

| Part | Provided by |
| --- | --- |
| A trigger | An event subscription (built). Time-based triggers are events too, emitted by a `scheduler` extension (not built), since a plugin cannot wake itself. |
| An action | Host capabilities, and the `tools` capability to call other plugins' tools. Such calls pass through the gates and are logged like the model's, with the plugin as their source. |
| Optionally, the model | The `llm` capability for one request, or the `sessions` service to start a run. |

What the core must guarantee, because the author is now the agent (built):

- **No impersonation.** A plugin loads a child under a name of its choosing, and a
  plugin's stored state and its place as a caller go by name. The plugin tree therefore
  never applies a plugin whose name another loaded plugin holds: a child named after an
  existing plugin fails to start (`plugin/failed`) instead of reading that plugin's state.
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
  protected from the start. Every plugin loaded through a runtime (children included) is
  refused them by `fs-read` and `fs-write`, whatever its grant says. The rest is the host's
  to add with `WasmRuntime::protect`: the library cannot know where the log or the grant
  files are, so whoever opens them protects them (the `corduroy` command does), and then a
  plugin cannot edit its own grants.
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
harnesses' extensions are written in; the Rust plugins are the reference, not the expected
norm. Each has an SDK (`sdk/js`, `sdk/python`); see [Guest languages](guest-languages.md#the-sdks-step-8). There are two ways to run them, and they suit different authors:

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
  server over stdio) asks for one through a kernel (`Kernel::spawn`), so the same grant and
  the same sandbox apply.
- **Native plugins are trusted.** A native plugin runs with the host's authority and no check
  applies to it. Anything that comes from a third party, or that the agent writes for
  itself, must be WebAssembly.

Which of these a person gets without setting anything up is decided in the next section.

## What is safe by default

Three things are decided here: what 1.0 promises about safety, what a kernel is given of the
machine it runs on, and what a workspace may configure. All of it is *planned*. The code is
as [Sandboxing](#sandboxing) above and the [security model](explanation/security-model.md)
describe it, and [Order of work](#order-of-work) lists what changes.

**The promise.** What the agent can reach is what you allowed, whatever the model is told
and whatever a workspace holds. Nothing it builds or loads gives it more.

**What is protected first is the harness.** The things that enforce come first: the core,
the grants and the record. If those hold, the agent and the workspace are held by them. A
hostile workspace cannot make the agent do what its grants do not allow, and an agent that
has been turned cannot change more than it was allowed to. Because the agent extends itself,
the weight is on the promise's second sentence: it may build and load more, and it never
ends up with more than the person gave it.

**Who is trusted.**

| | Trusted? | What follows |
| --- | --- | --- |
| The model | no: it may be an adversary, and it makes mistakes | Whatever it can do through its tools, someone who gets text in front of it can do: a web page, a file, a tool's result, a hint file. So the limits are on what its tools can reach, never on what it is told. |
| Add-ons | no, whoever wrote them, the agent included | They do only what their grants allow. |
| What tools return and what a workspace holds | no | It is text the model reads, and a workspace's agent file is a request (below). |
| What a kernel starts: a command, an MCP server over stdio | no | It is confined by the kernel that started it. |
| The core | yes | It is what enforces. |
| Extensions | yes | All of them are ours until after 1.0. How an extension someone else wrote comes to be trusted is a question for then. |
| The person, their machine and their account | yes | The harness cannot defend against the account it runs under. |
| The model's provider | yes, for now | It sees what it is sent. A model served on this machine is the way out. |

**Mistakes are contained, not undone.** What is hard to recover from is made hard or
impossible to do, by what is granted. A mistake then stays inside what was allowed, and the
record shows what was done. Restoring files in the workspace is the person's version
control, and catching a dangerous command before it runs is a gate's work, which is policy.
Snapshots of a kernel come after 1.0.

**Two sandboxes, and one of them is ours.** An outer one wraps the whole harness: a
container, a micro-VM, a sandbox runtime. It is the person's choice, and nothing here
promises it or relies on it. The inner one is the harness's own: capabilities for add-ons,
and for what a kernel runs, the operating system's confinement (Seatbelt on macOS,
bubblewrap or Landlock on Linux) inside a kernel extension. It cannot be an add-on, since no
capability starts a process.

**The default agent's shell is confined.** The confined kernel is built as one of the proofs
[the freeze waits on](#what-the-freeze-waits-on). It then ships in the standard set and is
the default agent's shell.

- *The agent's shell is a subset of the person's.* It can be widened as far as their own,
  and only by them.
- *The plain shell stays*, chosen by name, for when an outer sandbox is the confinement:
  one sandbox inside another often does not work.
- *No silent downgrade.* Where a shell cannot be confined, the kernel does not start. The
  command says why and names the change that opts into the plain shell, which is the
  person's choice alone.
- *Linux and macOS are promised.* What confines a shell on Windows is still to be found
  (H10 in [Open questions](open-questions.md)); until then the shell there is opt-in. The
  file tools work everywhere, since they are add-ons.

**A kernel is given things as an add-on is granted them.** Nothing by default, and each
thing listed, in the patterns every scope uses:

| | The default agent's shell |
| --- | --- |
| Files it writes | The workspace, apart from its agent file. |
| Files it reads | The workspace, and the system's program and library directories, without which no command runs. Nothing of the home directory, so a toolchain installed there is a line the person adds. |
| Hosts it reaches | None to begin with. The list grows by the person's answers. |
| Environment variables | A few plain ones the kernel sets itself (`PATH`, `HOME`). Nothing of the command's own environment. |

Other sandboxes made for agents let a command read everything unless told otherwise. Here a
read is granted like a write, so the person decides which credentials on the disk are
within reach. The default is just enough to start work with and no more. What is refused
is recorded, so the person sees what else was wanted and adds it: a refused host by name,
and a refused file as far as the operating system says which one it was, which the proof has
to find out.

**Every kernel starts from an empty environment**, the plain shell included, since that
needs nothing from the operating system. Today the shell starts with the command's own
environment, which is where the provider's key is.

**The shell reaches only the hosts listed for it.** The list is written as an add-on's
`http` grant is. The operating system offers a network that is on or off, so the kernel
passes the shell's traffic through a filter of its own. That filter is a condition of 1.0,
though it is work in the extension and none in the core: with only on and off, people leave
it on and the promise is empty.

- *The person does not write the list by hand.* A connection to a host not on it is
  refused and recorded with the host's name, and the person is offered it. A yes allows it
  from then on.
- *A refused connection fails the command.* It does not hold the command while the person
  decides; the model tries again after the answer. A call that is running cannot wait for a
  person today, which is the same gap as approving a load (C15), and the two are closed
  together.
- *What 1.0 says* is that the shell reaches only the hosts the person allowed. It does not
  say that data cannot leave: the filter tells hosts apart and not requests, so an allowed
  host can be sent anything.

Every host the agent can reach is then written down: in an add-on's `http` grant, in a
kernel's list, or as the address of a provider or an MCP server in an agent file.

**One path for every credential.** A provider's key and an MCP server's token come through
`Secrets`, as an add-on's secrets do, so a keychain in place of the environment covers all
of them. The person's own environment is where they come from by default (`env-secret`). A
credential is then in one of three states as far as a shell is concerned:

| State | What the shell has | How it gets there |
| --- | --- | --- |
| Absent | Nothing. | The default for everything but the plain variables. |
| Masked | The name, set to a placeholder. The filter puts the value in its place in requests to the hosts the secret names, and nowhere else. | It can be asked for while the agent runs. |
| Passed through | The value. | Only by the person writing it into an agent file, never by an ask. |

- *Masked is in 1.0.* It gives the shell what add-ons already have: a handle and never the
  value. `git push` works, and printing the variable shows the placeholder.
- *A secret says which hosts it may go to.* `Secret` is the core's, so this is in the core
  before the freeze (B9). It also keeps a provider's key from following a `base_url` that
  an agent file pointed somewhere else.
- *The filter reads what it writes into.* It needs a certificate of its own that programs
  in the sandbox trust, and it should read only the traffic to hosts a masked secret names,
  passing the rest through unread.
- *Only a key sent over HTTPS can be masked.* An SSH key or a database password is passed
  through or absent.
- *The agent asks, and the person answers.* The agent asks for a credential by name,
  through a tool. It never gives itself one, and it is not shown what the person's own
  environment holds.
- *Passing a value through is the person's line in an agent file.* The model can then
  print it, and what it prints is in the log.

**Two places.** What a person has and what a project has are kept apart:

| Place | Whose | Holds |
| --- | --- | --- |
| Account level | The person's. Outside every workspace, and out of reach of every shell and add-on. | Their own agent file, their answers, and for each workspace its log and its compiled-code cache. |
| Project level | The workspace's: `corduroy.toml` and `.corduroy/`. | The project's agent file, and what the agent builds there: skills, memories, its add-ons. It may be committed and shared. |

- *What enforces is never the agent's to write:* the grants, the log, the command and the
  add-ons shipped with it, the compiled-code cache, the person's answers. Not through an
  add-on (protected paths already cover the agent file, the log, the command and the
  cache), and not through a shell.
- *What the agent builds is.* Writing a skill or an add-on grants nothing: loading one is
  still asked at `plugin/load`, within what its loader holds.
- *A clone brings text and no authority.* A repository someone else wrote can bring hint
  files, skills and memories, which the model reads and the grants bound. It cannot bring
  a log, compiled code or grants. The cost is that the record no longer travels with the
  project's folder.
- *Account-level things are not committed with a project.* A person may keep them in a
  repository of their own.

**A workspace's agent file is a request until the person accepts it.** Today it is read the
moment `corduroy` runs in the directory, so a repository someone else wrote configures the
agent on this machine.

- *The first time,* the command shows what the file asks for (what the shell reaches, the
  hosts, the add-ons, the MCP servers, where the model's API is) and the person accepts or
  declines. The yes is remembered by the file's hash, so a changed file asks again.
- *Declined,* the person's own agent runs in its place. *With nobody there to ask,* the run
  stops and says why.
- *A file named with `--agent`* counts as accepted: naming it is the person's act.
- *A shipped add-on's name resolves to the shipped one* (built). The agent file's directory
  is searched last, after the add-ons that ship with the command, so a workspace cannot
  stand in its own `approval.wasm`.

**One file is one whole agent.** There is no merging in 1.0, only a fallback: the file named
with `--agent`, else the workspace's accepted file, else the person's own, else the built-in
default. The first one found is the whole agent. The person's own file is what says "always
this provider" in every workspace.

**The person's answers are theirs.** Accepted files, allowed hosts, masked credentials and
add-ons switched off are kept at account level, for each workspace, and never written into
the project's file: that would change its hash, and put one person's answers into a
repository others share. So files do not merge, and answers adjust: the agent that runs is
the accepted file, plus what the person allowed, minus what they switched off. Two people on
one project can then differ, one with the `memory` add-on and one without. A command lists
the answers and, when the person wants, writes them into a file.

The exception is an add-on that guards a decision point (`approval`, `inspect`). Taking a
guard away widens what the agent may do, so it is not switched off by an answer: that takes
an agent file of the person's own.

**Six guarantees, each with a test.** The promise is kept by these, and none is claimed
without a test that would fail if it stopped being true. The [security
model](explanation/security-model.md) becomes the list of them, with the test for each.

| | Guarantee | Today |
| --- | --- | --- |
| 1 | An add-on does only what its grants allow. | Built. |
| 2 | The shell reaches only the files, hosts and variables listed for it. A shell that cannot be confined does not start unless the person chose the plain one. | Not built. |
| 3 | What enforces is out of the agent's reach: the grants, the log, the command and its add-ons, the person's answers. | For add-ons, protected paths cover the agent file, the log, the command and the cache. Not built: the shipped add-ons, the answers, and anything for a shell. |
| 4 | Whatever the agent builds or loads holds at most what its loader holds, and a person approved the load. | No amplification and the gate are built. A person cannot answer yet: an ask at `plugin/load` is a denial (C15). Not built: a session with grants of its own, which this guarantee is read as covering ([What an extension can reach](#what-an-extension-can-reach)). |
| 5 | A workspace changes nothing about what the agent may do until the person accepts its file. | Not built. |
| 6 | No credential's value reaches an add-on. A shell holds a value only where an agent file the person wrote or accepted says so, and a masked one goes only to the hosts its secret names. | Built for add-ons, not for a shell. |

**What is not promised.**

- *That data cannot leave.* An allowed host, and the model's provider, can be sent anything
  the agent can read.
- *That a mistake inside the workspace can be undone.* It is contained, as above.
- *Anything about the plain shell, an extension, or the machine and the account.*
- *That a gate catches everything.* Gates are policy: `inspect` is a heuristic, and
  `read-only` is a tool's own claim.

**A fix may break.** After 1.0, a flaw that breaks one of the six may be fixed by a change
that breaks a frozen contract, announced as one. How it is announced and how long a release
is supported are still open (A11), as is whether a review from outside is a condition of 1.0
(P9).

## Shipped plugins: the Goose baseline

Each row is a plugin, with the core primitive it waits on. Nothing here is built into the
harness.

| Goose feature | Plugin here | Kind | Waits on |
| --- | --- | --- | --- |
| Developer: `write`, `edit`, `shell`, `tree`, `read_image` | `pi-tools` (`tree` added) | WebAssembly | `read_image` waits on reading a file as bytes (`fs-read` reads text); tool results already carry images |
| MCP extensions (stdio, streamable HTTP) | `corduroy-mcp` | native (`rmcp`) | done: stdio servers start through a kernel |
| Providers | `corduroy-providers` (`oi-kit-providers`) | native | done |
| Permission modes, per-tool permissions | `approval` (exists: `auto`, `smart`, `approve`, `chat`, allow and deny lists) | WebAssembly | done |
| Tool inspection, prompt-injection scan | `inspect` (exists: dangerous commands, credentials, repetition, injected instructions in results) | WebAssembly | done |
| Auto-compaction | `compact` (exists) | WebAssembly | done: the projection knows the `compaction` event |
| Summon: `load`, `delegate` | `subagents` (exists: `delegate`) | WebAssembly | done for `delegate`; `load` (a recipe into the context) is not built |
| Skills | `skills` (exists) | WebAssembly | done; the review fork waits on sessions |
| Context files (`.goosehints`, `AGENTS.md`) | `hints` (exists) | WebAssembly | done |
| Todo | `todo` (exists) | WebAssembly | done |
| Memory | `memory` (exists) | WebAssembly | done |
| Chat Recall | `recall` (exists) | WebAssembly | done |
| Summarize | `summarize` (exists) | WebAssembly | done |
| Extension Manager | `plugin-manager` (exists) | WebAssembly | done |
| Code Mode | `code-mode` | WebAssembly + kernel | kernel-to-host bridge |
| Recipes and sub-recipes | the agent file of `corduroy` | host configuration | done for one agent; sub-recipes wait on sessions |

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
7. *Done,* but for `session`. `provider` as a service (`Providers`, the `provider` plugin, a
   loop that looks its provider up on every request); `ShellKernel` out of the core into
   `corduroy-shell`; and `llm`, `secret`, `http` and `log-read`, with the `Models`,
   `Secrets` and `HttpClients` registries behind them. Checked from JavaScript and Python,
   see [Guest languages](guest-languages.md#reaching-out-llm-secret-http-and-log-read).
   *Still to do:* `session`, after general waits and signals; an HTTP client plugin.
8. *Done.* Guest languages. The spike ran early and settled the WASI question (build
   without it); then, as phase three's first part: `corduroy-guest`, the crate every Rust
   plugin builds on; an SDK for JavaScript and TypeScript (`sdk/js`) and for Python
   (`sdk/python`), each generating a world from a capability list, routing logging, clocks
   and randomness to host calls and mapping exceptions to tool errors; the script host as a
   shipped component (`sdk/js/script-host`); and the same example plugin in both languages,
   built in CI and loaded by the host's tests. See
   [Guest languages](guest-languages.md#the-sdks-step-8).
9. Shipped plugins. *Phase three, second part,* aiming at a version that can be tried out
   with a real model:
   - *Done:* the `corduroy` command (`crates/corduroy-cli`) and its agent file, the first
     form of grant files: one TOML file naming the model, the providers, kernels, secrets
     and WebAssembly plugins with exactly what each is granted. The log and the agent file
     are protected paths, so no plugin can rewrite the record or its own grants. Providers
     come from `oi-kit-providers` (`crates/corduroy-providers`), the `http` capability
     sends through `crates/corduroy-http`. A gate's ask is answered at the terminal, or, with
     nobody there, waits in the log for `corduroy continue`.
   - *Done:* the plugins that waited on nothing: `hints`, `todo`, `memory`, `recall`,
     `summarize`, `inspect`, `plugin-manager`, and `tree` in `pi-tools`, each a Rust guest
     of under 120 KB, all but `plugin-manager` in the default agent.
   - *Done:* MCP. A server over stdio is a long-lived process with pipes, which
     `Kernel::exec` cannot hold, so kernels gained `Kernel::spawn` (refused by default;
     `ShellKernel` starts the process in its directory and process group): the rule that
     processes start only through a kernel stays, and a sandboxed kernel sandboxes MCP
     servers too. `crates/corduroy-mcp` is a native plugin per server, stdio or streamable
     HTTP, registering its tools as `<server>__<tool>`; `[mcp.<name>]` in the agent file.
   - *Done:* compaction. The projection understands a `compaction` event and hides what
     it summarises from the model (`Session::for_model`), keeping it in
     the log and the conversation. `CompactOperation`, before the prompt step, asks the
     plugin providing the `compaction` service whether a session is due and for a summary
     of what came before the current turn; the `compact` plugin answers with a model of its
     own through `llm`.
   - *Done:* sub-agents. The general form of waits and signals (a tool call that waits
     for a named signal, in the log), child sessions started through the harness's
     `sessions` service, and the `subagents` plugin's `delegate`.

   Left for later: code mode (the kernel-to-host bridge) and `read_image` (needs reading
   a file as bytes; tool results can carry an image).
10. *Done.* A review of the core before calling it stable, which found and fixed: events
    had no time; two plugins could share a name, and with it a state store; decision points
    were a closed list in the core; loop steps could be added only by the host, not by a
    plugin that can be unloaded; tool results were one string. With it, the extension
    tier: `[extensions]` in the agent file, and the command as a library so a binary can
    add kinds. See [Three tiers](#three-tiers-core-extensions-add-ons) and
    [What is stable](#what-is-stable).
11. *Done.* Three things the review left open. The conversation's shape is the core's
    `Message`, and the loop is the harness's own, with oi-kit kept behind the provider
    boundary. A plugin can ask which session it serves (the `session` capability), and
    `todo` keeps a list per session. A run can be stopped, at any point, and Ctrl-C in the
    `corduroy` command does it.
12. What [the record promises](#what-the-record-promises). *Done:* the read side of `Log`
    and paged reads that may fail, which the harness, the WebAssembly host and the command
    read through; removing a session through the command (`corduroy sessions remove`),
    with its marker, and readers that tolerate what is gone (the JSON Lines store reads
    ids with gaps); `process/start`, what each process runs as; and `EventStream::replay`,
    which loads a log and re-executes nothing, renamed `EventStream::load`. *Still to do:*
    the folder store, as the second store the freeze waits on; then the live channel, a
    streaming method on `Provider`, output from a running tool and checkpoints, tried
    against a real provider before their shapes are fixed.
13. What [the core is made of](#what-the-core-is-made-of). *Done:* the one request and
    reply in the core, with usage that counts reasoning and the reason a model stopped;
    `Model` taking them, and a plugin's calls recording their usage and that reason; models
    named `<provider>/<model>` and the `serve` key gone; a model's description, from the
    provider and the agent file, read by the loop and by `compact`. *Still to do:* the
    general typed part; a cost on the usage, once its unit is decided; the `llm` interface
    carrying the whole request and reply and listing the models a grant reaches, once the
    form is (E8); the form of a question a tool puts to the person; a way for an extension
    to raise a signal and to start a session. Then the connected harness, the proof that
    calling and waiting is enough.
14. What is [safe by default](#what-is-safe-by-default). *Decided, none of it built:* a
    secret that names the hosts it may go to, in the core before the freeze; every kernel
    starting from an empty environment, and every credential through `Secrets`; the
    confined kernel for Linux and macOS, given its files, hosts and variables as an add-on
    is granted them, with its filter for hosts and masked credentials in the shell; the
    default agent on that kernel, and a shell that cannot be confined not starting; the
    account-level place, holding the person's own agent file, their answers, the log and
    the compiled-code cache, and a name for it in the agent file's strings; a workspace's
    agent file accepted by its hash, and the fallback order of agent files; a test named for
    each of the six guarantees. *Done:* a shipped add-on's name resolved before anything a
    workspace holds.
15. How [the plugin world grows](#how-the-plugin-world-grows). *Decided, none of it built:*
    a check of the world's version when an add-on is loaded, with a message naming the
    version it was built for, the host's and what is missing; each capability's sentence
    read again before the freeze; the interfaces already known to be wanted (progress from
    a running tool, a tool call that waits for a signal) in the 1.0 world; the cases and
    open tails the shared types need, since none can be added within 1.x; a prefix for the
    service names the harness defines; a reference page for the script host's `host`
    object; a built add-on from each released world as a fixture every later host loads.
    To check on the way: that an add-on importing no capability still says which world it
    was built for, in each guest language, and that the three toolchains accept a WIT in
    which a function says which version it arrived in.
16. What [an extension can reach](#what-an-extension-can-reach). *Decided, none of it
    built:* in the core before the freeze, the call scope carrying a session's grants, a
    kernel saying whether it can hold a session to them, and a `Kernels` registry that does
    not lock a whole kernel; `session/start` carrying a model, a loop, instructions, grants
    and limits, with `session/start` declared as a decision point and a session keeping
    what it started with; host calls checked against the add-on's grant and the session's;
    a shell for each session, confined to both; the `sessions` service raising a signal,
    starting a session and sending a message from anywhere, with signals named
    `<plugin>/<what>` and a message for a busy session queued in the log; the driver, and
    the command's three ways of running it; an instance of an add-on for each session;
    children of one message run together; the session's snapshot readable, a single tool
    call with its trail on `Run`, a context for `Operation::run`, and `Run` no longer
    handing out the harness; `write` and `edit` refusing a file that changed since it was
    read.

## Open questions

What must be decided before 1.0 is gathered in
[Open questions before 1.0](open-questions.md), each as a question with a direction and no
answer. The list below is this record's own, older and broader: an entry that has to be
settled by 1.0 is there as well, and one that can wait is only here.

- **Grant files.** The `corduroy` agent file is the first form: one file naming the plugins
  of an agent and what each may do, as Goose's recipes and Cordis's `cordis.yml` do. Still
  open: a grant file per plugin shipped with it (Zed's `extension.toml`, Tauri's capability
  files), so a plugin states what it wants and the agent file only narrows it.
- **Scrubbing the log.** If the model does write a sensitive value into a tool call, the
  assistant message holding it is committed before any gate is asked, so neither a deny nor a
  rewrite keeps it out of the log. If that matters it is a property of the log (a filter
  applied to a batch before it is stored), not of gates.
- **Session tree.** Sessions are linear for now, with a parent link. Pi's sessions are a tree
  of entries, so one session can branch from any earlier point, which gives rewinding and a
  common starting point for several attempts. Because every event names its session and a
  session names its parent, branching can be added as "a new session whose parent is event N
  of another" without changing the log.
- **Operations from plugins.** Loop steps are native `Operation`s, which extensions
  register in `Operations`. A WebAssembly plugin will not get an export for one: the
  exports are closed at six ([How the plugin world grows](#how-the-plugin-world-grows)). It
  says what a run's steps are through a service the harness asks, or as a description a
  native step carries out; which is still open.
- **Delivery order across threads.** Commits are stored in one order, and each is
  delivered by the thread that made it, so two threads committing at once may deliver out
  of id order. Listeners that must not get order wrong read the log. A single delivery
  queue would fix it at the cost of a commit returning before its listeners ran.
- **Progress from a running tool.** A tool can learn that its run is stopping, but cannot
  say how far it has got while it runs; its result comes when it returns. An event it
  emits is the way today. Decided, not built: output reported through the call scope goes
  to the live channel and into checkpoints
  ([What the record promises](#what-the-record-promises)). Still open is how an add-on
  reaches it.
- **Providers of our own.** `corduroy-providers` still builds on oi-kit-providers, an
  alpha, for the HTTP side of each model's API. It is confined to that crate, behind the
  harness's `Provider`, so replacing it (with clients of our own for the two or three APIs
  that matter, or with another crate) touches nothing else.
- **Scopes below a service.** The `service` capability is scoped by service name, so a
  grant gives all of a service's methods. `service:method` resources would let a grant
  name one.
- **Listeners and re-entry.** An event a native tool emits while a WebAssembly plugin's
  call is in progress is delivered at once, and a listener in that same plugin is refused
  (it is running), so it misses the event. Events plugins raise themselves are queued and
  unaffected. A per-plugin queue of undelivered events would close the gap.
- **Exports every guest must write.** Each export of the world is mandatory: the
  JavaScript and Python toolchains refuse to build a plugin without `gate`, and Rust's
  bindings will not compile. For Rust, `corduroy-guest` supplies a default for every export
  but `activate` (a gate that is asked anyway denies); the JavaScript and Python SDKs do
  the same. Decided: it stays so, and no export is added in 1.x. A host that tolerates a
  missing export could come later without breaking a built add-on
  ([How the plugin world grows](#how-the-plugin-world-grows)).
- **Blocking.** Tool calls run on blocking threads, but event listeners run inline on the
  thread that emitted, including inside the async loop. Slow listeners would need a queue.
- **Durable loads.** Plugins loaded by a plugin are not reloaded after a restart unless their
  loader does it. A plugin store (components by hash, with the grants they were loaded with)
  would let the host do it, and is also where grant files come in.
- **Approving a load.** `plugin/load` cannot ask a person yet; see Automations.
- **Shipping precompiled plugins.** wasmtime's cache removes the cost of every load but the
  first on a machine. Removing that one too means distributing compiled code, which is read
  back with the `unsafe` `Component::deserialize`. Not needed now; if it ever is, that is the
  place for one documented exception in a crate of its own.
