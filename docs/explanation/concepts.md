# Concepts

This page is the mental model: what the parts of corduroy are, how they fit, and why it is
built that way. [Where the ideas come from](inspirations.md) explains the projects each idea
is borrowed from, and the [design record](../design.md) goes deeper on each point.

## A small core, and everything else plugged in

Most agent harnesses are a program with an extension mechanism bolted on: the tools, the
prompt and the loop are built in, and plugins add to the edges. corduroy turns that round. The
**core** is a small, fixed piece that provides a plugin tree, an event stream, a loop and
registries for plugins to fill, and holds the rules (what is logged, what is checked, who may
do what). It ships no tools and no prompt of its own. Everything an agent can do arrives as a
**plugin**:

- the model (a provider plugin),
- the tools (`read`, `write`, `bash`… are an add-on, `pi-tools`),
- the text of the system prompt (prompt parts from plugins),
- the places code runs (kernels),
- the policies that decide what may happen (gates),
- reactions to what happened (event subscribers).

The aim is that whatever the plugins become, written by people or by the agent itself, the
same core is underneath enforcing the same rules.

## Three tiers

| Tier | What | Gets in by | Trusted with |
| --- | --- | --- | --- |
| **Core** | the plugin tree, capabilities, event stream, registries, the loop, and the [plugin world](../../wit/corduroy.wit) | always there | everything; it is the contract |
| **Extensions** | native Rust plugins: kernels, HTTP clients, model providers, MCP, loop steps | compiled into the command | the host's authority |
| **Add-ons** | WebAssembly plugins: tools, prompt text, gates, automations | a `.wasm` file, at start or while running | only what they are granted |

*Plugin* means either of the last two. The line between them is **how they get in, not what
they do**: an extension needs the command rebuilt, an add-on is dropped in. Trust follows
from that. Whatever can be added without a rebuild is sandboxed; whatever holds the host's
authority was compiled in by whoever built the binary.

So: write an add-on by default, and always for anything from a third party or written by the
agent. Write an extension only when you need something no capability gives, such as starting
processes, opening sockets or a native library.

## Plugins are reversible

Every registration a plugin makes (a tool, a prompt part, a listener, a service) is an
**effect** recorded on that plugin. Unloading it runs the effects backwards, so it leaves
nothing behind. A plugin that depends on a service another provides is suspended when that
service goes and applied again when it comes back. This comes from
[Cordis](inspirations.md#cordis-plugins-that-clean-up-after-themselves), and it is what lets
plugins be loaded and unloaded while an agent runs.

## Capabilities

An add-on starts with no authority at all: no files, no network, no clock. It can do only
what its imports allow, and each of the [sixteen imports](../reference/capabilities.md) is a
**capability** the host must **grant**, with a **scope** (which paths, which kernel, which
hosts). There is no manifest: what an add-on needs is what its component imports, and an
add-on missing a grant is never started. Each call is then checked against the scope, and a
refusal is logged.

## The event stream is the record

Every step of a run, every tool call and result, every gate decision, every plugin starting
or failing, is an **event** on one append-only stream. Each step's events are one
**commit**, written to the log *before* any listener or the next step sees them. The
conversation the model sees is a projection of `message` events, not state kept elsewhere.

This is what makes runs **durable**. Kill the process at any point and the log says exactly
how far the run got, down to each tool call: never started, running, or finished.
`continue` picks up from there, repeating a call only if its tool declared that safe. The
same is true of a run *waiting*: for a person's answer, or for a sub-agent to finish. The
wait is in the log, so it outlives the process.

## The loop

A run is a loop of **steps**. Each step looks at the conversation and either does something
(its effects become one commit) or passes to the next. The standard order is: operations
extensions added, compaction, the gates, the tools, waiting for a signal, the turn limit, and
finally asking the model. The loop itself is a plugin-provided part (`Loops`), so a different
order (plan first, review before answering) is a swap, not a fork. What every run must keep,
whatever loop takes it (stopping, waits, child sessions, recording how it ended), stays in
the harness.

## Gates decide, events observe

What other harnesses call hooks are two different things here:

- **An event** is fire-and-forget. Whoever emits it gets nothing back. Reacting (counting,
  logging, nudging the prompt) is subscribing.
- **A gate** is asked at a declared **decision point** (`tool/call`, `plugin/load`, or one an
  extension declares) before something happens, and answers allow, ask or deny. The
  strictest answer of all gates wins, so the outcome does not depend on load order, and the
  decision is logged before the thing happens. A gate that cannot answer denies.

An "ask" stops the run until a person answers, which can be much later. A denial goes back to
the model as an error with a reason, so it can try something else.

## Sessions

One log holds many **sessions**, each its own conversation. Every event of a run names its
session. A session can be started from another for a task (a sub-agent, through the
`sessions` service); the parent's tool call waits, in the log, for the child to end, and the
child's answer becomes its result.

## Services: plugins extending plugins

Typed Rust values cannot cross into WebAssembly, so plugins talk to each other through
**services**: named, JSON in and out, with the caller's name attached. An add-on that needs
something no capability gives (a database, a browser) calls a service an extension provides,
and its grant names that service. That is how extensions make add-ons more capable without
widening the sandbox: an add-on never gains authority, only something to ask.

## Glossary

| Term | Meaning |
| --- | --- |
| **add-on** | a WebAssembly plugin, sandboxed and granted capabilities |
| **agent file** | `corduroy.toml`: an agent's model, extensions and add-ons with their grants |
| **capability** | one host interface an add-on may import; granted with a scope |
| **commit** | one step's events, stored atomically before anyone sees them |
| **decision point** | a named place where gates are asked: `tool/call`, `plugin/load`, … |
| **extension** | a native plugin compiled into the command |
| **gate** | a plugin asked at a decision point; answers allow, ask or deny |
| **grant** | a capability given to an add-on, with the scope it reaches |
| **kernel** | a long-lived process (a shell) that runs code and keeps state between calls |
| **operation** | one kind of step in the standard loop |
| **plugin** | anything plugged into the core and unpluggable again: an extension or an add-on |
| **provider** | the plugin that serves a model to the loop |
| **scope** | allow and deny patterns over a capability's resources; deny wins |
| **service** | a named JSON call one plugin answers for others |
| **session** | one conversation in the log, possibly started from another |
| **signal** | what a waiting tool call waits for, such as a child session's end |
| **workspace** | the folder an agent works in |
