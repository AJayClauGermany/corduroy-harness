# Where the ideas come from

corduroy is not a port of any one harness. It takes **one mechanism** from each of six
projects, chosen because that mechanism solves a problem the others leave open, and builds it
on a single core. None of the six harnesses is used whole, and only one (Goose's oi-kit, for
model providers) is a dependency at all.

This page explains each borrowed idea: what the original project is, what the idea is and why
it matters, how it works here, and what was deliberately left behind. The
[design record](../design.md#the-six-inspirations) has the shorter, engineering-facing
version, with the research dates.

| Project | The idea taken | Where it lives here |
| --- | --- | --- |
| [Cordis](#cordis-plugins-that-clean-up-after-themselves) | every registration is an effect that unloading reverts; plugins follow the services they depend on | `corduroy-core::context` |
| [Prime Intellect](#prime-intellect-a-kernel-that-remembers) | a long-lived kernel the model computes in, keeping state between calls | `corduroy-core::kernel`, `corduroy-shell` |
| [Hermes](#hermes-skills-the-agent-writes-for-itself) | skills the agent writes, loads and repairs for itself | `plugins/skills` |
| [Pi](#pi-four-tools-and-a-durable-run) | a minimal toolset; and, from Pi Durable, durability as a property of the loop | `plugins/pi-tools`, `src/session.rs` |
| [OpenHands](#openhands-the-log-is-the-truth) | an append-only event stream as the source of truth; the conversation as a view of it | `corduroy-core::event`, `src/session.rs` |
| [Goose](#goose-a-loop-of-steps-and-a-baseline) | the loop as a list of steps over the persisted conversation; its providers; its feature set as the baseline | `src/loops.rs`, `crates/corduroy-providers`, `plugins/` |

---

## Cordis: plugins that clean up after themselves

**The project.** [Cordis](https://github.com/cordiverse/cordis) is a TypeScript plugin
framework from the Koishi chat-bot ecosystem, describing itself as "a meta-framework of
spatiotemporal composability". [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)
vendors it and builds every subsystem (tools, the model client, the agent loop) as a Cordis
plugin providing a service; its
[Cordis primer](https://deepseek-harness.github.io/deepseek-harness/en/reference/cordis-primer)
is the best introduction.

**The problem it solves.** In most plugin systems, loading a plugin is easy and unloading it
is not. The plugin registered a tool here, a listener there, started a timer, provided a
service someone else now uses. Unless every plugin author writes a perfect teardown, removing
a plugin leaves debris behind, so in practice plugins are loaded at start and never removed.
That rules out what corduroy is for: an agent that loads, replaces and drops plugins while it
runs, including plugins it wrote itself.

**The idea.** Cordis makes composability work along two axes:

- **In time ("temporal").** A plugin never touches shared state directly. It is handed a
  *context*, and everything it registers through that context (a listener, a service, a
  child plugin) is recorded as an *effect* with a disposer, owned by the plugin's *fiber*
  (its loaded instance). Unloading the plugin runs the disposers in reverse order. Cleanup is
  not something the author writes; it falls out of how registration works.
- **In space ("spatial").** A plugin declares the services it needs (`inject`). It is not
  applied until they all exist. If one goes away, the plugin is suspended (its effects
  reverted) before the service goes, and applied again when the service returns. Plugins
  follow their dependencies instead of crashing on a missing one.

**How it works here.** `corduroy_core::context` is a Rust rendering of the same model. `App`
is the tree of fibers; `Context` is the only door a plugin has to shared state; every
registry's `register(ctx, ..)`, `ctx.on`, `ctx.provide`, `ctx.plugin` and `ctx.effect`
record effects:

```rust
let shouter = harness.plugin(plugin_fn("shouter", &[TOOLS], |ctx| {
    // Each registration is an effect recorded on this plugin.
    let tools = ctx.service::<Tools>(TOOLS)?;
    let spec = ToolSpec::new("shout", "Upper-case the input.", r#"{"type":"object"}"#);
    tools.register(ctx, "shout", Arc::new(tool_fn(spec, |input| Ok(input.to_uppercase()))))?;
    ctx.on("turn/end", |_event| { /* react to every finished run */ });
    ctx.effect(|| { /* any other clean-up, run when the plugin goes */ });
    Ok(())
}));
assert!(harness.tools().get("shout").is_some());

// Unloading runs the effects backwards: the listener, the tool, the clean-up all go.
harness.app().dispose(shouter)?;
assert!(harness.tools().get("shout").is_none());
```

`&[TOOLS]` is the `inject` list: asking for a service not listed there is refused, as in
Cordis, so a plugin's dependencies are always declared. A plugin whose `apply` fails halfway
is rolled back the same way as one that is unloaded.

Three things are stricter here than in Cordis, because the plugins may be untrusted:

- **A name belongs to one plugin.** Events, stored state and service calls identify a plugin
  by name, so a second plugin cannot take a name already held, or one of the reserved names
  (`host`, `agent`, `user`, `environment`). A plugin cannot impersonate another.
- **The tree changes only when it settles.** A plugin's own call cannot reshape the tree it is
  part of; loads it asks for are queued (`Requests`) and applied before the next step.
- **WebAssembly plugins get the same guarantees.** An add-on's tools, prompt part, gate and
  subscriptions are effects of its fiber, so unloading an add-on, or the plugin that loaded
  it, takes all of it away.

**Left behind.** `ctx.isolate` (a separate instance of a service for one subtree),
configuration-driven plugin trees (the agent file plays that part), async `apply`, and Cordis's
other event dispatch modes (`waterfall`, `parallel`, `serial`, `bail`). Only plain emit
exists, on purpose: `bail`, where the first listener to return a value decides, would let a
permissive plugin loaded early overrule a strict one. Decisions go through
[gates](concepts.md#gates-decide-events-observe) instead, where the strictest answer wins
whatever the load order.

---

## Prime Intellect: a kernel that remembers

**The project.** [Prime Agent](https://github.com/PrimeIntellect-ai/prime-agent) is Prime
Intellect's open-source coding and research agent, built around their work on
[recursive language models](https://www.primeintellect.ai/blog/rlm). Its README puts the core
choice plainly: "a persistent Python REPL is the built-in model tool; file operations, shell
commands, tool use, subagents, and context management happen through code."

**The problem it solves.** A typical agent's shell tool starts a fresh process for every
command. Everything the model worked out is forgotten when the command exits: the directory it
moved into, the variables it set, the data it loaded, the server it started. So the model
re-derives state on every call, or crams long pipelines into one command, or pastes
intermediate results back into its own context, which is the most expensive memory it has.

**The idea.** Give the model a *kernel*: one long-lived interpreter process whose state
persists from one call (a *cell*) to the next, like a notebook. In Prime Agent the REPL is a
Python subprocess speaking newline-delimited JSON
([protocol](https://github.com/PrimeIntellect-ai/prime-agent/blob/main/prime-agent-runtime/src/rlm/repl.md)),
running every cell in one persistent namespace, with interrupts, caps on output, snapshot and
restore of the namespace, and a bridge that lets code in the kernel make requests back to the
host. The model builds up a computation over many steps instead of starting over each time,
and keeps large data in the kernel rather than in its context.

**How it works here.** A kernel is a core trait, `corduroy_core::Kernel`: run a cell with a
timeout, get back its output, its status and whether the output was cut short (past 1 MiB).
Kernels are registered by name in the `Kernels` registry. The core ships none; `corduroy-shell`
provides `ShellKernel`, a persistent POSIX shell (or PowerShell), and the agent file names
them under `[kernels.*]`. The `bash` tool in `pi-tools` is a thin layer over the `kernel`
capability, so state carries over between the model's calls. From the event stream
`cargo demo` prints:

```text
#13 main [agent] tool/call {"id":"2","tool":"bash","input":{"command":"lines=$(wc -l < notes.txt)"}}
#16 main [environment] tool/result {"id":"2","tool":"bash","ok":true,"output":""}
#19 main [agent] tool/call {"id":"3","tool":"bash","input":{"command":"echo \"notes.txt has $lines lines\""}}
#22 main [environment] tool/result {"id":"3","tool":"bash","ok":true,"output":"notes.txt has 3 lines\n"}
```

The second call sees the variable the first one set. A cell that times out, or a shell that
exits, costs the kernel its state, and the error says so, so the model knows to set things up
again.

Two things here go beyond Prime Agent's REPL:

- **The kernel is the security decision.** Kernels are named resources behind a capability,
  so an add-on is granted *which* kernels it may run code in. And a kernel is the *only* way
  to start a process: MCP servers over stdio start through `Kernel::spawn`. A sandboxed
  kernel (a container, bubblewrap) therefore sandboxes everything that runs code, with no
  other path around it.
- **An add-on is a small kernel too.** A WebAssembly plugin's memory lives as long as it stays
  loaded, so a plugin can hold state between calls the same way, within its sandbox.

**Left behind, for now.** A Python kernel (any interpreter can be one; a shell came first
because every machine has one), snapshot and restore (planned as optional `Kernel` methods so
a kernel's state can survive a crash), and the kernel-to-host bridge, which would let code in
a cell call the harness's tools directly ("code mode"). Prime Agent's sub-agents run through
its REPL; here they are child sessions (the `subagents` add-on), so that they are logged,
gated and resumable like any other run.

---

## Hermes: skills the agent writes for itself

**The project.** [Hermes Agent](https://github.com/NousResearch/hermes-agent), by Nous
Research, bills itself as "the agent that grows with you": a self-improving agent with a
built-in learning loop. Its central mechanism is
[skills](https://hermes-agent.nousresearch.com/docs/user-guide/features/skills).

**The problem it solves.** An agent that works out how to do something (release the project,
deploy to staging, run the flaky test suite properly) forgets it when the session ends. The
next session pays again to rediscover it, and may get it wrong. Writing everything into the
system prompt does not scale: the prompt grows with every lesson, and most of it is
irrelevant to any given task.

**The idea.** Let the agent write down procedures as **skills**, files it can read, create and
fix itself, and disclose them progressively:

- A skill is a directory holding a `SKILL.md`: front matter with a `name` and a `description`,
  then a body saying how to do the thing. Hermes keeps them as `<category>/<name>/SKILL.md`,
  compatible with the [agentskills.io](https://agentskills.io) format.
- **Progressive disclosure.** Only an index (each skill's name and description) is in the
  system prompt. The model loads a skill's body (`skill_view`) when the index suggests it is
  relevant. A hundred skills cost a hundred lines of prompt, not a hundred documents.
- **The agent maintains them.** `skill_manage` creates, patches and deletes skills, with a
  targeted patch preferred to a rewrite, so a wrong step is corrected rather than
  duplicated.
- **A learning loop.** Every so often (about every ten turns, per its docs) Hermes forks a
  background review of the conversation that asks whether anything learned should become or
  improve a skill. A curator later ages out skills nobody uses.

**How it works here.** `plugins/skills` is an add-on that holds `fs-read` and `fs-write` on
its own skills directory (`.corduroy/skills` in the default agent) and `events` on
`skills/*`, and nothing else. It offers `skills_list`, `skill_view` and `skill_manage`
(`create`, `patch` by one unique string or by whole content, `delete`), validates names and
front matter, and contributes the index to every system prompt:

```text
Skills are procedures you saved earlier. Before starting a task, check the index below and
load any skill that looks relevant with skill_view(name). …
<available_skills>
- count-lines: Count the lines of a text file
</available_skills>
```

The learning loop is in-band rather than a fork. The add-on subscribes to `tool/result` and
`turn/end`; once `nudge_interval` tool calls (10 by default) pass without a skill being
saved, it emits `skills/review-due` and adds a `<skill_review>` reminder to the prompt until
one is. The event is the hook a separate reviewer could attach to.

What corduroy adds is the sandbox around the idea. Skills are text the model *reads*; the
plugin that writes them is confined by its grants to one directory, and the same approach
extends to code the harness *runs*: an agent can write an [add-on](../tutorials/first-add-on.md)
or a script-host automation, and it runs with only what it is granted. See
[Automations](../design.md#automations).

**Left behind, for now.** The background review fork (it waits on nothing now that child
sessions exist, and `skills/review-due` is where it would attach), supporting files beside
`SKILL.md`, categories, usage tracking and the curator, and Hermes's linting, scanning and
write approval of agent-written skills. Approval has a natural home here already: a gate on
`skill_manage` calls.

---

## Pi: four tools, and a durable run

**The project.** [Pi](https://github.com/earendil-works/pi) is a deliberately minimal coding
agent. Its default tools are four: `read` (with `offset` and `limit`), `write`, `edit` (a
list of exact `oldText`/`newText` replacements, each unique, all matched against the
original) and `bash`, with output capped at 2000 lines or 50 KB. Everything else is an
extension hooking typed events.

**The idea taken.** A small, sharp toolset. Each tool a model does not have to choose between
is prompt space and attention saved, and these four are enough for real work.
`plugins/pi-tools` has the same tools, schemas and limits, plus `tree` from Goose so an agent
without a shell can find its way. It is one add-on among others, not a built-in: the harness
has no tools of its own.

**A second idea, from Pi Durable.** Pi's experimental durable runtime (`@earendil-works/pi-durable`)
stores each step of a run so it survives a crash: every change is one atomic commit; nothing
is shown before its commit is stored; a tool call's intent is committed before it runs, and
after a crash it is repeated only if the tool declared that safe; waits survive restarts; a
restart limit turns a crash loop into an error. Pi Durable is a separate runtime with its own
loop, and the lesson taken is exactly that: **durability belongs in the loop and the log, not
in an extension.** corduroy's loop is durable for every run. See
[Durability](../design.md#durability) and [Stop, resume and recover runs](../how-to/recover-runs.md).

**Left behind.** Fuzzy matching in `edit`, saving cut-off `bash` output to a file, per-tool
prompt snippets, Pi's session tree (branching a session from any earlier point; the log could
grow it without a format change), and hooks that rewrite a call's input (gates deny with a
reason instead; [why](../design.md#events-gates-and-hooks)).

---

## OpenHands: the log is the truth

**The project.** [OpenHands](https://github.com/OpenHands/software-agent-sdk) is an open
platform for software agents. Its first version was built around a pub/sub `EventStream` of
actions and observations; the V1 SDK made events immutable and typed, the agent stateless,
and the conversation a *view* over an append-only log, with condensation recorded as an event
rather than a rewrite of history.

**The idea taken.** The append-only event stream is the single source of truth. Here every
event has a source, the time its commit was stored, an optional cause and, if it belongs to
one, a session. The conversation the model sees is a projection of `message` events
(`src/session.rs`); compaction is a `compaction` event that hides old messages from the model
without deleting them. Plugin lifecycle, capability refusals, gate decisions and
plugin-defined topics share the same stream, so the model, the plugins and the person reading
`corduroy log` see one history. See [Events](../reference/events.md).

**Elsewhere here.** OpenHands's security analyser and confirmation policy correspond to the
`inspect` and `approval` gates.

---

## Goose: a loop of steps, and a baseline

**The project.** [Goose](https://github.com/aaif-goose/goose) is an open-source agent whose
building blocks are published as the `oi-kit` crates. `oi-kit-agent` unrolls the agent loop
into an ordered list of operations, each of which looks at the persisted conversation and
either declines or returns effects to persist, with asking the model as the last step.
`oi-kit-providers` holds HTTP clients for Anthropic, OpenAI, Ollama and many more.

**The ideas taken.**

- **The loop as steps.** The harness's own loop (`StandardLoop`, about 300 lines) has the same
  shape: at each step it tries the host's operations, those extensions registered,
  compaction, the gates, the tools, waiting for a signal, the turn limit and the model, and
  the first that does something is the step, committed as one batch. The loop itself is
  swappable by name. See [The loop](../design.md#the-loop).
- **The providers, behind a wall.** `corduroy-providers` uses `oi-kit-providers` for the
  HTTP side of each model's API and converts to and from the harness's own `Message` at the
  boundary, so no oi-kit type reaches the loop, the log or a plugin. An earlier version used
  oi-kit's loop and message types directly; that tied the log format to an alpha crate, and
  was undone.
- **The baseline.** What a user gets from Goose out of the box should be reachable here with
  shipped plugins alone. Each Goose feature maps to an add-on or extension:
  developer tools to `pi-tools`, MCP extensions to `corduroy-mcp`, permission modes to
  `approval`, its inspectors to `inspect`, auto-compaction to `compact`, summon to
  `subagents`, `.goosehints` to `hints`, and so on through `todo`, `memory`, `recall`,
  `summarize` and `plugin-manager`. See [Shipped add-ons](../reference/shipped-add-ons.md)
  and the [baseline table](../design.md#shipped-plugins-the-goose-baseline).

**Left behind.** oi-kit's own MCP, compaction and sub-agent operations, in favour of plugins
over the core's primitives that are logged, gated and unloadable like everything else; and
Goose's desktop-only features (apps, computer control, the auto-visualiser).

---

## Further reading

| Project | Start with |
| --- | --- |
| Cordis | [repository](https://github.com/cordiverse/cordis); [primer](https://deepseek-harness.github.io/deepseek-harness/en/reference/cordis-primer) and [registry API](https://deepseek-harness.github.io/deepseek-harness/en/reference/cordis-api/registry) in DeepSeek Harness's docs |
| Prime Agent | [repository](https://github.com/PrimeIntellect-ai/prime-agent); [REPL protocol](https://github.com/PrimeIntellect-ai/prime-agent/blob/main/prime-agent-runtime/src/rlm/repl.md); [recursive language models](https://www.primeintellect.ai/blog/rlm) |
| Hermes Agent | [repository](https://github.com/NousResearch/hermes-agent); [skills](https://hermes-agent.nousresearch.com/docs/user-guide/features/skills); [curator](https://hermes-agent.nousresearch.com/docs/user-guide/features/curator) |
| Pi | [repository](https://github.com/earendil-works/pi) (`packages/durable` for Pi Durable) |
| OpenHands | [software agent SDK](https://github.com/OpenHands/software-agent-sdk) |
| Goose | [repository](https://github.com/aaif-goose/goose) (the `oi-kit-*` crates) |

The external projects move quickly; the descriptions here were checked against their
sources in November 2026.
