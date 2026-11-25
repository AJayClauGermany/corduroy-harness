# Open questions before 1.0

Every decision that has to be made before the core is called stable. At 1.0 the core is
frozen: it gets bug fixes and little else, and extensions and add-ons are written against it
as it stands. A question is here because answering it later would mean changing something
that will by then be frozen, or something extension authors will by then rely on.

**This page holds questions, not answers.** Each entry says what has to be decided, why it
cannot wait, where the code stands today, and a *direction*: a recommendation to argue with,
not a decision. When a question is decided, the answer is written where it belongs
([Design](design.md), the [reference](README.md#reference) pages, the code's own docs) and
the entry is deleted from this page. An empty section is a finished one.

Defects found on the way (things that are simply wrong, with nothing to decide) are not
listed here.

| Section | What it settles |
| --- | --- |
| [P. Purpose and place](#p-purpose-and-place) | what this is for, for whom, and what it offers that the others do not |
| [A. What the freeze is](#a-what-the-freeze-is) | which contracts freeze, under which rule, and how that is kept |
| [B. Core: types and traits](#b-core-types-and-traits) | `corduroy-core`'s shapes |
| [C. Core: the event stream and the log](#c-core-the-event-stream-and-the-log) | what an event is and what the record promises |
| [D. Core: gates, capabilities and scopes](#d-core-gates-capabilities-and-scopes) | how decisions and grants are expressed |
| [E. The plugin world](#e-the-plugin-world) | `wit/corduroy.wit` |
| [F. The WebAssembly host](#f-the-webassembly-host) | `corduroy-wasm`'s Rust API and limits |
| [G. The loop and the extension tier](#g-the-loop-and-the-extension-tier) | what extensions implement in `corduroy-harness` |
| [H. The standard set, the command and the agent file](#h-the-standard-set-the-command-and-the-agent-file) | `corduroy-std`, `corduroy-cli` |
| [I. Add-ons and the guest SDKs](#i-add-ons-and-the-guest-sdks) | the little that must be fixed there |

Paths are relative to the repository root; `core/` stands for `crates/corduroy-core/src/`.

## Where to start

The questions are not equally weighty. The ones below set the direction; most of the rest
become small, or answer themselves, once these are decided. Take a group at a time, decide
it, then read its sections again before moving on, because an answer here often removes or
reshapes entries further down.

| Order | Group | Questions |
| --- | --- | --- |
| 8 | How people meet it | H1, H11, H14, I4 |

Everything not in this table is detail that hangs off one of these, or a mechanical choice
(which types may grow, which names are final) best made in one sitting per section once its
group is settled. The first seven groups are decided, and every question below is read
against them. Why this exists, and the two uses 1.0 is measured against, are in
[What it is for](design.md#what-it-is-for). What freezes, what is only stable and what
"frozen" allows are in [What is stable](design.md#what-is-stable), with the four proofs
[the freeze waits on](design.md#what-the-freeze-waits-on). Running without the standard
library is a direction and not a promise:
[Without the standard library](design.md#without-the-standard-library). What the log is
for, what is promised about resuming, live output, removal and reading are in
[What the record promises](design.md#what-the-record-promises). What a model call is, what
a model says about itself, that the core's traits are called and waited for, and that the
kinds of thing a plugin contributes are closed at eight are in
[What the core is made of](design.md#what-the-core-is-made-of). What 1.0 promises about
safety, what a kernel is given of the machine, and what a workspace may configure are in
[What is safe by default](design.md#what-is-safe-by-default). What is promised to an add-on
already built, how the capabilities grow, that the exports are closed at six, and which
versions of the world a host accepts are in
[How the plugin world grows](design.md#how-the-plugin-world-grows). What a session is
started with and held to, what something outside a run may do, who takes a run on, what
runs at once, and what a step an extension wrote may read and write are in
[What an extension can reach](design.md#what-an-extension-can-reach).

## P. Purpose and place

There are many agent harnesses, and this one is assembled from ideas found in six of them.
These questions ask what it is for, so that the freeze fixes a foundation for something in
particular. They have no code to point at; where they lean on a claim, it is one the
project's own pages make. The first of them are decided, in
[What it is for](design.md#what-it-is-for); the ones left are narrower.

### P5. How is an MCP server held to what it was allowed?

[What it is for](design.md#what-it-is-for) treats a tool from an MCP server and a tool from
an add-on alike, and lets the agent build either. They are not alike in what they can do: an
add-on runs with its grants and nothing else, and a stdio MCP server is a process started
through a kernel, with whatever that kernel can reach.
[What is safe by default](design.md#what-is-safe-by-default) decides what that is: the
default agent's kernel is confined, so a server it starts, one the agent wrote included, is
held to the files, hosts and variables that kernel was given. A server reached over HTTP is
an address in an agent file. MCP has authorization and a way for a server to ask the person
for input; whether either can stand where a gate does has not been checked.

A session's grants do not reach inside a server either
([What an extension can reach](design.md#what-an-extension-can-reach)). A session's shell
is confined to what the session is granted, but a server is started for no session and
keeps all the kernel was given, so its tools are offered to a narrowed session or not, by
name, and whoever offers one has decided what that session reaches through it.

*Direction.* Decide whether one kernel's reach is the right grain, since every server a
kernel starts shares it with the shell, or whether a server gets a kernel of its own. Read
the MCP specification before saying its approvals and the gates are one thing. Then the
traffic the other way: whether this harness's tools are offered to other agents over MCP.
Nothing needs it yet.

### P6. Do six borrowed mechanisms add up to one thesis?

Each idea was taken because it "solves a problem the others leave open". That explains each
part and not the whole. Without a problem that needs all six solved together, the result is
a well-made collection.

*Direction.* Write the thesis without naming any of the six projects, then check each
borrowed idea against it. One that the thesis does not need (kernels, skills, running
without the standard library and the Goose baseline are the ones to test) is a candidate to
demote from the core's story to "also possible".

### P7. Is another harness's feature list the right measure?

"What a user gets from Goose out of the box should be reachable here with shipped plugins
alone." As a test that the plugin system is expressive enough, that is a good one. As a goal
it makes this a follower: always matching, a release behind, a list someone else writes.

*Direction.* Keep parity as the test of expressiveness. State separately what a person gets
here that the baseline does not give them, and let 1.0 be judged on that.

### P9. Is the fixed core itself the offer?

"The core is meant to be fixed: compiled, small, and not something an agent or a plugin can
change. … whatever the plugins become, the same core is underneath and still enforcing the
same rules." That is an assurance argument: a small trusted base an organisation can read,
audit and rely on while everything above it moves.

What is promised is decided: one statement and six guarantees, each with a test
([What is safe by default](design.md#what-is-safe-by-default)). Who checks them besides us
is not.

*Direction.* Decide whether assurance is the pitch. If it is, the size of the trusted base
becomes a feature with a budget (lines, dependencies, the WebAssembly engine included), and
a review of the six guarantees from outside is a condition of 1.0 rather than a
nice-to-have.

### P10. Who writes add-ons: people or the agent?

The design says the purpose of the plugin system is that the agent adapts the harness to its
task. The SDKs, tutorials and reference are written for people. The two authors want
different things: a model wants a script it can write in one go, a tight error when a grant
refuses it, and a person to approve the load; a person wants packaging, versions and
somewhere to publish ([I4](#i4-what-is-an-add-on-as-something-shipped)).

*Direction.* Name the primary author. It decides which of the script host, the SDKs and a
package format gets finished first.

### P11. Is the plugin world ours alone, or meant for other hosts?

A frozen, versioned, typed interface with capabilities as imports is close to a
specification. If another host could implement it, an add-on would be portable between
harnesses, which nothing offers today and which would be a contribution to the field rather
than to one project. If it is ours alone, it is an internal boundary with good hygiene.

*Direction.* Decide which. A specification needs a second implementation as proof and a
home that is not one repository; an internal boundary needs neither, and should not carry
the cost of pretending.

### P13. Is independence from any one model part of the value?

The agents most people use are made by model vendors and move with their models. An
independent harness is neutral and always a little behind on what one model newly offers.
The evals here already run against a 1.7-billion-parameter model on a laptop.

*Direction.* Decide whether "works well with any model, small and local ones included" is a
stated goal. It shapes the default prompts, the tool set, the loop and what counts as a
regression.

### P14. How does it reach people?

A new terminal agent asks someone to change a habit. The alternatives are to sit behind
something they already use (an editor, through a protocol editors and agents share), or
inside something someone else ships. [H11](#h11-is-there-a-front-end-other-than-a-rust-program)
asks about the mechanism; this asks about the intent.

*Direction.* The purpose is a harness a person uses, with the command line as its front end
and adoption not a goal of 1.0 ([What it is for](design.md#what-it-is-for)). Leave this
until there is someone other than its author to reach.

### P15. What will show that it is worth having?

Nothing outside this repository depends on it yet. Before the core is frozen for good,
something should show the purpose is real: a task done here that cannot be done, or cannot
be done safely, elsewhere; an extension written by someone who is not us; a user who stayed.

*Direction.* One demonstration that is the thesis made runnable, chosen before 1.0 and
passed before it. The test-driven pair among the proofs
[the freeze waits on](design.md#what-the-freeze-waits-on) is the candidate; decide whether it
is the demonstration or only a proof that the surface is enough.

### P16. Who keeps it going after 1.0?

A frozen core is most useful if someone answers for it: reviews fixes, holds the line on
what does not change, speaks for the world's next version. Whether that is a company, a few
maintainers or a foundation changes what "stable" is worth to someone building on it.

*Direction.* Say who, and what they commit to, together with
[A11](#a11-what-may-still-change-after-10).

## A. What the freeze is

[What is stable](design.md#what-is-stable) says which contracts are frozen and which stable,
and that a frozen one grows only where growth was marked before 1.0. Sections B to E are
therefore a list of which places to mark. What is left here is how the freeze is kept.

### A4. What enforces the freeze?

Nothing does today: CI has no check of the public API, the WIT or the log format against a
recorded baseline. One check is decided: a built add-on from each released world is kept as
a fixture and loaded by every later host
([How the plugin world grows](design.md#how-the-plugin-world-grows)).

*Direction.* `cargo-semver-checks` (or a public-API snapshot) on `corduroy-core` and
`corduroy-harness`; a snapshot of `wit/corduroy.wit`'s exports, shared types and each
interface's functions, so that a changed one fails and an added one is seen; a log file of
every schema version from 1.0 on kept as a fixture and read back in a test. The same check
can hold the list of places marked for growth, so that an addition anywhere else fails. For
the add-on fixtures, decide which guest languages are kept and where: a JavaScript or
Python add-on is 12 to 19 MB, which is a lot to hold in the repository for every release.

### A5. What carries the version 1.0, and are the versions tied?

The crates are `0.1.0`, the WIT package is `corduroy:plugin@0.1.0`, the log's schema is `2`.
Whether these move together decides what a guest SDK, a store and an extension each pin.
What a version of the world means is decided, and an add-on built against its 1.0 loads on
every host whose world is 1.x
([How the plugin world grows](design.md#how-the-plugin-world-grows)).

*Direction.* One version per contract, moving independently: the crates (semver), the WIT
package, the log schema. Then say whether the world may reach a new major version while
the command stays at 1.x, since a person reads "every 1.x" as the command's.

### A6. Is the minimum Rust version part of the contract?

The workspace asks for Rust 1.97 and edition 2024. An extension author has to know whether a
core bug-fix release may raise that.

*Direction.* State a policy before 1.0 (for example: a raise is a minor release, never a
patch).

### A10. What is published, where, and under which names?

Every crate has `publish = false`. Nothing is on crates.io, npm or PyPI, and there is no
built command to download. The licence is `BSL-1.0`, the Boost licence.

*Direction.* Decide names and channels before 1.0, since a crate's name is the first thing
to freeze, and confirm the licence is the one meant.

### A11. What may still change after 1.0?

One thing is decided: a flaw that breaks one of the six guarantees may be fixed by a change
that breaks a frozen contract, announced as one
([What is safe by default](design.md#what-is-safe-by-default)).

*Direction.* Say how such a fix is announced, how long a 1.x release is supported, and
whether anything but a broken guarantee may break a frozen contract.

### A12. Is there a conformance kit?

A store, a kernel, a provider and an HTTP client written by someone else each have rules to
keep (a store keeps a batch whole or not at all; a client follows no redirects; a kernel
ends its cell on a stop) and nothing to test themselves against. `corduroy-test-support`
is internal.

*Direction.* A public test suite per trait an extension implements, in the stable set.

## B. Core: types and traits

### B1. Which public types are closed on purpose?

Fourteen public types cannot gain a field or a case. Each needs a deliberate answer: closed
for good, or `#[non_exhaustive]` with a constructor.

| Type | Where | Direction |
| --- | --- | --- |
| `Event` | `core/event.rs:146` | Open. Design says events gain fields; a store builds one when reading a log back, so it also needs a constructor. |
| `Source` | `core/event.rs:101` | Decide with [B21](#b21-are-the-reserved-names-and-event-sources-a-closed-set). |
| `Role` | `core/message.rs:44` | Decide: are `user` and `assistant` the only roles, with tool results said by the user, for good? |
| `ToolError` | `core/tool.rs:241` | Open (a stopped or timed-out call has no case). |
| `CallError` | `core/service.rs:31` | Open. |
| `ContextError` | `core/context.rs:187` | Open. |
| `Verdict` | `core/gate.rs:179` | See [D1](#d1-is-a-verdict-allow-ask-or-deny-for-good). |
| `Decision` | `core/gate.rs:231` | Open (the other gates' verdicts may be wanted). |
| `Cell`, `Spawn` | `core/kernel.rs:43,134` | See [B6](#b6-what-does-a-cell-return-and-what-can-a-spawned-process-be-given). |
| `HttpRequest`, `HttpResponse` | `core/http.rs:24,49` | See [B8](#b8-is-an-http-exchange-always-one-whole-body). |
| `Denied` | `core/capability.rs:158` | Open, or closed on purpose. |
| `Duplicate` | `core/registry.rs:22` | Closed is fine; confirm. |

### B2. Can the struct-shaped variants grow?

`Part::Image`, `Part::Thinking` (`core/message.rs:52,60`) and `Content::Image`
(`core/tool.rs:117`) are in `#[non_exhaustive]` enums, but their own fields are fixed: a
provider that returns something more about its thinking, or an image with a name, cannot be
represented without breaking every match.

*Direction.* Mark the variants `#[non_exhaustive]`, or make each a struct with a constructor
as `ToolCall` and `ToolResult` are. The general typed part
[What the core is made of](design.md#what-the-core-is-made-of) adds (what a model that is
not a chat model returns: a vector, a yes or a no) is made the same way. Decide what it
holds: a media type and bytes, or JSON text.

### B5. Are there kernels without the standard library?

`kernel` is compiled only with `std` (`core/lib.rs:77`), and `Kernels` is
`Registry<std::sync::Mutex<dyn Kernel>>` (`core/kernel.rs:35`). Design promises a device "a
kernel that is an embedded interpreter"
([Without the standard library](design.md#without-the-standard-library)), which the core
cannot express.

One part is decided ([What an extension can reach](design.md#what-an-extension-can-reach)):
the registry stops holding a whole kernel behind one lock, since a kernel keeps a shell for
each session and sessions run side by side. So the standard mutex leaves the registry's
type either way, and `Kernel` gains a defaulted method that says whether it can hold a
session to its grants.

*Direction.* A device is not promised by 1.0, so this cannot hold the freeze up, but the
shape of `Kernels` freezes with the core. With the lock moved inside each kernel, either
split the trait (`exec` and `reset` everywhere; `spawn` and `Process` with `std`), or say
that kernels need an operating system and take the sentence out of Design.

### B6. What does a cell return, and what can a spawned process be given?

A `Cell` is text, a status and a truncation flag (`core/kernel.rs:43`): no image or other
rich output, which a notebook-style kernel produces; no separate error stream; nothing
until the cell ends. A `Spawn` has a program, arguments and environment
(`core/kernel.rs:134`): no working directory. A `Process` has no error stream and no exit
status. Every kernel is to start from an empty environment
([What is safe by default](design.md#what-is-safe-by-default)), so the environment a `Spawn`
names is all a process gets.

*Direction.* Make `Cell` and `Spawn` growable now. Decide separately whether rich output
belongs in 1.0; the trait can gain defaulted methods later.

### B8. Is an HTTP exchange always one whole body?

`HttpClient::send` returns the whole response or a `String` error
(`core/http.rs:63-71`). There is no stream (server-sent events, a large download) and no
way to tell a timeout from a refused connection. `HttpRequest` and `HttpResponse` cannot
grow.

*Direction.* Make both structs growable and give the error a type. Leave streaming for a
defaulted method later, and confirm that is acceptable for an MCP client written as an
add-on.

### B9. How does a secret say where it may be sent?

`Secret` is `reveal()` and nothing else (`core/secret.rs:20`). A plugin granted a secret
and a broad `http` scope can send that secret to any host its scope reaches.

That a secret names the hosts it may go to is decided, and it is in the core before the
freeze ([What is safe by default](design.md#what-is-safe-by-default)): a masked credential
in a shell is written only into requests to those hosts, and a provider's key no longer
follows a `base_url` pointed somewhere else.

*Direction.* A defaulted `hosts()` on `Secret`, checked wherever a value is written into a
request: the host for an add-on, a kernel's filter for a shell, a provider for its key.
Decide what a secret that names no host means (it goes nowhere, or anywhere), and how the
agent file states the hosts.

### B10. In what order are prompt parts rendered?

By name (`core/prompt.rs:31-43`), all into the system prompt, afresh on every request. A
part cannot say it belongs first or last, or that it changes on every request, so a
volatile part early in the alphabet defeats a provider's prompt cache for everything after
it.

*Direction.* Decide whether name order is the contract. If not, add a defaulted placement
method before 1.0 so the order has a growth point.

### B11. Are tools one flat, global set?

`Tools` is one registry for the whole agent, with no record of which plugin registered an
entry.

That it stays one flat set is decided: the tools a session is offered are its `tools` grant,
a scope over the one registry, and not a registry of its own
([What an extension can reach](design.md#what-an-extension-can-reach)).

*Direction.* What is left is ownership: confirm that which plugin registered a tool can be
added later as a new method.

### B12. What does a pattern mean, for good?

`*` matches any run of characters, `/` included; there is no other syntax and no escape
(`core/pattern.rs:6-9`). Patterns are written in agent files as grants, so any syntax added
later (`**`, `?`, an escape) changes what an existing grant reaches.

*Direction.* Declare the syntax final, or reserve the characters a later syntax would use
by refusing them now.

### B13. Should two public types both be called `Scope`?

`capability::Scope` (what a grant reaches) and `call::Scope` (what a thread is serving).
Only the first is re-exported at the root.

*Direction.* Rename `call::Scope` before the name is frozen.

### B14. Do the closure implementations stay?

`Gate`, `Service`, `Model`, `Secret`, `HttpClient` and `PromptPart` are each implemented
for every matching closure. That rules out later implementing them for `Arc<T>` or `Box<T>`.

*Direction.* Keep them; record the trade.

### B15. Are text-only errors enough?

`LogError` and `PluginError` are opaque strings, which can gain a `kind()` later. The
errors of `Secret::reveal` and `HttpClient::send` are bare `String`s in a trait signature,
which cannot.

*Direction.* Give the two trait errors a type before 1.0.

### B16. How does a native plugin emit an event for a session?

`Context::emit` attaches no session (`core/context.rs:834`), and exists only during
`apply`. Afterwards a native plugin holds the stream itself, names its own `Source` and
attaches the session by hand; an add-on gets both from the host.

A connected harness is the first that needs it: the other side's progress is to be seen
here as events of the session it works for
([What the freeze waits on](design.md#what-the-freeze-waits-on)), and it arrives on the
extension's own thread, outside any call.

*Direction.* An emitter bound to the plugin's name that takes the session from the call
scope, or is told it when there is no call, so there is one convention before extensions
settle on their own.

### B17. Does the call scope follow work to another thread?

It is thread-local (`core/call.rs:56`). An extension that hands a tool call to another
thread loses the session, the waits and the stop flag, and there is no way to carry them.

It now loses more. The scope is to carry what the session is granted
([What an extension can reach](design.md#what-an-extension-can-reach)), so work done on a
thread without it is done under no session's limits at all.

*Direction.* A handle that captures the current scope and enters it elsewhere, in the core
before the freeze. An add-on is to have an instance for each session, so its own host calls
keep their session whatever the thread; what loses it is native code that calls a tool or a
kernel from a thread of its own. Say what such a call is served as.

### B19. Is polling the only way to learn of a stop?

`call::stopping()` is asked "now and then". Nothing wakes work that is blocked. A plugin
whose call has returned, and which waits to raise a signal, hears of a stop as an event
([What an extension can reach](design.md#what-an-extension-can-reach)).

*Direction.* Confirm; a notification can be added.

### B21. Are the reserved names and event sources a closed set?

`RESERVED_NAMES` is `host`, `agent`, `user`, `environment` (`core/context.rs:46`), matching
`Source`. A name reserved after 1.0 could already belong to someone's plugin.

*Direction.* Close `Source` on purpose, or reserve a prefix now for sources yet to come.

### B22. Does a plugin, or its stored state, have a version?

`Plugin` has a name and nothing else; the `state` store is keyed by that name. Design lists
"a version on each plugin's stored state" as still to do. An add-on that changes its state's
shape has nowhere to record which shape a log holds.

*Direction.* Decide before the log freezes, since the answer is a field on `state/set` or a
convention inside the value.

### B25. Whose job is a tool's input, and the size of its result?

Neither the core nor the loop checks a call's input against the tool's schema, or bounds
what a result adds to the conversation. Each tool does, or does not.

*Direction.* Say it is the tool's, or give the loop a bound: one runaway result can use up
a session's context.

### B26. How are tool names kept apart?

There is one flat set of names. An MCP server's tools are `<server>__<tool>`; an add-on's
are bare, and the second to register a name is refused.

*Direction.* A naming rule before third-party add-ons exist, and a decision on what a clash
does to the plugin that lost.

### B27. Can code in a kernel call back into the agent?

Code mode ("left for later" in the design) needs it: "the kernel-to-host bridge". Whether
that is a method on `Kernel`, a service, or a socket the process talks to decides whether
the core changes. Kernel snapshots (planned as defaulted `snapshot` and `restore`) raise
the same kind of question.

*Direction.* Spike both before the freeze, to show each is an addition.

### B28. Is there one person?

`Source::User` has no identity. Nothing records who sent a message or who answered an ask.

One case is decided: a message a plugin sends a session is recorded with who sent it, so a
review tells it from the person
([What an extension can reach](design.md#what-an-extension-can-reach)). Its form is not: a
field on `message`, or a source of its own
([B21](#b21-are-the-reserved-names-and-event-sources-a-closed-set)).

*Direction.* Decide the form for a plugin's message first, since it is in the log. Then
whether a person's identity is a field the front end puts in the payload of `message` and
`gate/answer`, and whether that is enough for a host with many users.

## C. Core: the event stream and the log

### C3. In what order are commits from two threads delivered?

Each is delivered by the thread that made it, so a listener may hear the later first
([Design, open questions](design.md#open-questions)). One delivery queue would fix the
order and change when a commit returns.

It matters more than it did: sessions are to run side by side, each one thing at a time
([What an extension can reach](design.md#what-an-extension-can-reach)), so two threads
committing at once is the ordinary case.

*Direction.* Decide which is the frozen behaviour. The least that fits the decision is
that one session's commits are heard in order, and nothing is said about two sessions'.

### C4. Do listeners run on the committing thread?

Yes, inline, the async loop included; a slow listener holds the run.

*Direction.* Decide with C3: a queue answers both.

### C5. May a plugin miss an event because it was busy?

An event a native tool emits while an add-on's call is in progress is not delivered to a
listener in that same add-on. A per-plugin queue of undelivered events would close it.

*Direction.* Decide whether delivery to add-ons is guaranteed, since automations depend on it.

### C6. Can anything be kept out of the log?

A sensitive value the model writes into a tool call is committed before any gate is asked.
A filter on a batch before it is stored would be a property of the log.

[What the record promises](design.md#what-the-record-promises) works from "everything that
can be logged is logged" and leaves this open. Review is part of the purpose, so what is
kept out is a product question: a private value in a prompt is also something the person
may not want kept. Once in, a value leaves only with its whole session. A credential the
person passed through to a shell is such a value as soon as the model prints it
([What is safe by default](design.md#what-is-safe-by-default)).

*Direction.* Decide whether the core offers a filter point, or whether a `Log` that wraps
another is the answer (listeners would still see the value). Checkpoints of live output
pass the same point, so one answer has to cover them.

### C7. Is an event's payload JSON by contract or by convention?

`Event::payload` is a `String`, "by convention JSON" (`core/event.rs:160`).

*Direction.* Make it the contract for every topic the core and the loop define; say what an
add-on's own topics may hold.

### C8. Who may emit which topics?

`events.emit` refuses `state/*` and nothing else. The session projection ignores events
whose source is a plugin, which is what stops a forged `message`, but the topic names
themselves are free for anyone.

*Direction.* Reserve the prefixes the core and the loop use, and give add-ons a rule
(`<plugin>/…`).

### C9. What is the frozen thing: the log file, or the event?

[What is stable](design.md#what-is-stable) names `corduroy-jsonl` and the topic table.
Another store (a database, flash) implements `Log` and never writes that file.

*Direction.* Freeze the event and its topics as the contract; the JSON Lines file is one
store's format with its own schema version.

### C11. Which topics and payload fields are frozen, and where is the one list?

The table is in `src/session.rs`, the reference is `docs/reference/events.md`, and the core
emits five topics of its own. They do not agree today (the reference lacks
`plugin/suspended` and `plugin/dropped`).

[What an extension can reach](design.md#what-an-extension-can-reach) decides several things
that are in the log and have no shape yet: what `session/start` holds (a model, a loop,
instructions, grants, limits, and who started the session), a message that waits for a run
to end, a signal kept for a wait not yet recorded, and an answer to an ask about a session.
A message that waits cannot be a `message` event until its run begins, since a run is read
as starting at the last message a person wrote.

*Direction.* One list, checked by a test, marked with which topics are contract and which
are diagnostics. Shape the new ones as they are built, under the rule that old logs still
read: a `session/start` that says nothing runs as the agent is configured.

### C12. Which spelling do JSON keys use?

Payloads and gate requests mix `read-only` (`src/gates.rs:65`) with `keep_from`,
`mime_type` and `input_tokens`. After 1.0 neither can be renamed.

*Direction.* Pick one before the log freezes.

### C13. What may a session id contain?

A `SessionId` is any string. Children are named `<parent>/<n>`, and `log-read` scopes are
`<session>:<topic>` with `%` and `:` escaped.

*Direction.* A grammar for ids, so `/` and `:` mean one thing. The folder store
([What the record promises](design.md#what-the-record-promises)) names a file after each
session, so the grammar also has to be safe as a file name on every platform promised, or
the store has to encode ids.

### C14. Can a session branch from an earlier point?

Sessions are linear with a parent link. Design holds that branching can be added as "a new
session whose parent is event N of another" without changing the log.

*Direction.* Prove that with a spike before freezing `session/start`, against the folder
store ([What the record promises](design.md#what-the-record-promises)), where a branch is a
new file whose start names an event in another. Say what a branch means once the session it
came from has been removed.

### C15. Can a person answer an ask at `plugin/load`?

Not yet (design, "still to do"). Asks are answered per tool call (`Harness::answer`).

One of the six guarantees waits on this: that a person approved what the agent loads
([What is safe by default](design.md#what-is-safe-by-default)). The same section has two
more things a person is offered while a call runs, a host the shell was refused and a
credential the agent asked for. Neither holds the call: it fails, the person answers, and
the model tries again.

A third point is decided to ask a person: a session a gate asked about at `session/start`
is recorded and waits, in the log, for the answer
([What an extension can reach](design.md#what-an-extension-can-reach)). A session started
from outside, which would run in the background, is the case.

*Direction.* Decide whether an ask at any declared point can wait for a person, since that
shapes `gate/answer`, which names a tool call today and has to name a session or a load as
well; and whether a refusal the person may lift afterwards is an ask of that kind or an
event of its own.

### C18. How many processes may use one log?

One. A log has one writer, and the file store locks the file when it opens it, so two
terminals cannot work in the same workspace at once.

*Direction.* Decide whether one process per log is the contract, with a service that owns
the log and serves the others
([H11](#h11-is-there-a-front-end-other-than-a-rust-program)), or whether readers may share.
A file per session ([What the record promises](design.md#what-the-record-promises)) does
not by itself allow two writers: ids are one sequence for the workspace.

### C20. Where do images and large results live?

In the log, as base64 inside the events that carry them.

Stored bytes are not a kind of thing plugins contribute
([What the core is made of](design.md#what-the-core-is-made-of)): where a store keeps a
large payload is the store's business, and there is no registry for it.

*Direction.* What is left is whether an event may name bytes it does not hold (a kind of
`Content` and `Part` that names a blob, which the log's store keeps). The kind can be added
later, but every log written before then holds the bytes. Removing a session
([What the record promises](design.md#what-the-record-promises)) would then have to remove
its bytes too, and bytes two sessions share would need an owner.

### C21. What does the stream hold once a store keeps part of the log back?

The read side is built ([What the record promises](design.md#what-the-record-promises)):
`Log::read` with a default, and `EventStream::read` and `read_where`, which are paged, may
fail, and are complete. The reads that cannot fail (`events()`, `session()`, `matching()`,
`after()`, `get()`) answer from memory, so they are complete only while the store hands the
stream everything, which the JSON Lines file does. The folder store will be the first that
does not, and four things are then unsettled:

- What the convenient reads promise: what the stream holds, and so less than the log; or
  the whole log, reaching the store and hiding a failure to read it.
- Which sessions a store hands over when it is opened ("the sessions in use"): those with
  an unfinished run, those the program names, or none, each read when first asked for.
- Whether the stream lets events go while it runs (a window, or sessions not touched for a
  while), and whether what `read` brings back from the store is then kept.
- Whether a plugin that reads with `log-read` should know that a read reached the store,
  since it is slower and can fail.

*Direction.* Decide with the folder store, before the core freezes, since the answer is
what `EventStream`'s reads promise for good. Keeping the convenient reads to memory and
saying so in their names (or their docs) costs nothing now; making them reach the store
later changes what they promise.

## D. Core: gates, capabilities and scopes

### D1. Is a verdict allow, ask or deny, for good?

`Verdict` is closed (`core/gate.rs:179`), and so is `verdict` in the world. Rewriting a
call was decided against. "Allow, and remember" is left to the gate's own memory.

*Direction.* Close it on purpose and say so, or leave room in both places.

### D2. Are the gate requests' JSON shapes contract?

`tool/call` and `plugin/load` requests are described in doc comments
(`core/gate.rs:33-41`).

*Direction.* Yes, under the same rule as events: fields are added, never removed.

### D3. Which decision points exist at 1.0?

The core declares two. Design names "the user's input and the request about to be sent to
the model" as the likely next. A point the loop asks at is the loop's to declare.

A third is decided ([What an extension can reach](design.md#what-an-extension-can-reach)):
the loop declares `session/start`, and the gates are asked with who is starting the session
(the person, a session's agent through a tool, a plugin), its parent, and what it would be
started with: its model, loop, instructions, grants and limits. Which model a child may run
on, and whether an orchestrator may start orchestrators, are then a gate's to say.

*Direction.* Decide whether the loop declares the other two before 1.0, since gates written
by third parties will key on the names. For `session/start`, what is left is the request's
JSON ([D2](#d2-are-the-gate-requests-json-shapes-contract)).

### D4. Is name order, first strictest, the final rule?

Gates are asked in name order and the first to give the strictest kind is recorded.

*Direction.* Confirm.

### D5. Can a grant name one method of a service?

The `service` capability is scoped by service name, so a grant gives every method.

`sessions` is now the case for it. Starting a child, starting a session from outside,
raising a signal and sending a session a message are four different things to allow, and
one grant gives all four
([What an extension can reach](design.md#what-an-extension-can-reach)).

*Direction.* Decide whether the resource is `service` or `service:method` before grants are
written against it.

### D6. Does a plugin ship a statement of what it wants?

Design's open question on grant files: the agent file grants; a plugin cannot say what it
needs beyond its imports.

*Direction.* Decide whether this belongs in 1.0's agent file contract or after.

### D10. What form does a tool's question to the person take?

A gate's ask is the only question a run puts to a person today. That a tool can ask
something else (which of these three branches, a value it lacks) is decided, and is in 1.0
([What the core is made of](design.md#what-the-core-is-made-of)): it is not a gate, which
decides whether something may happen, and not a registry, but a tool that asks and waits
for a signal. What no front end knows yet is how to show the question. The agent asking
for a credential by name is one such question
([What is safe by default](design.md#what-is-safe-by-default)).

*Direction.* Settle the form before front ends are written: the event's topic and what it
holds (the question, the choices if there are any), what a reply may be (a choice, a
value, a refusal to answer), the signal that carries it, and that a question with nobody
there waits in the log as an ask does. An add-on can provide the tool only if its call can
wait ([E6](#e6-can-an-add-ons-tool-call-wait-for-a-signal)).

### D12. Is there one place that says where an agent may send data?

`http` is scoped by host for add-ons, and a confined kernel is to reach only the hosts
listed for it ([What is safe by default](design.md#what-is-safe-by-default)). A provider and
an MCP server reached over HTTP are extensions, and go to the address an agent file gives
them. So every host is written down, but in three forms, and nothing is asked before a
request leaves.

*Direction.* Decide whether 1.0 wants a decision point for outbound requests, which
extensions and a kernel's filter are expected to ask at, or leaves it at the lists. An
allowed host can still be sent anything, which no list changes.

## E. The plugin world

### E1. What can a host call fail with?

`host-error` is `denied` or `failed` (`wit/corduroy.wit:6`), used by fourteen interfaces. A
missing file, bad input and a timeout are all `failed`. A variant gains no case within 1.x:
the engine checks types, so an add-on already built would stop loading
([How the plugin world grows](design.md#how-the-plugin-world-grows)).

*Direction.* Add the cases now, since there is no later: at least `not-found`, `invalid`,
`timed-out`.

### E2. Can what a guest returns grow?

`tool-spec` and `plugin-info` are fixed records, though the core's `ToolSpec` was made
growable. `content` is text or an image. None of them gains a field or a case within 1.x
([How the plugin world grows](design.md#how-the-plugin-world-grows)), so whatever is to
grow has to be given its place before 1.0.

*Direction.* An open tail on both records (a list of named values), and a general typed
block (`mime-type`, `data`) beside `image`.

### E5. Can a header mix text and a secret?

A `header-value` is all text or all secret (`wit/corduroy.wit:333`), so
`Authorization: Bearer <secret>` works only if the stored secret includes `Bearer `.

*Direction.* A value as a list of text and secret parts.

### E6. Can an add-on's tool call wait for a signal?

Natively, yes (`call::wait_for`). The world has no such call; an add-on waits only by
starting a child session.

[What the core is made of](design.md#what-the-core-is-made-of) leans on waiting twice: a
tool that asks the person a question, and an agent that waits for the next message from
another. If an add-on cannot wait, both have to be extensions.

Where it goes is decided: waiting changes how a run proceeds, which no grant of `session`
covers, so it is an interface of its own, in the 1.0 world
([How the plugin world grows](design.md#how-the-plugin-world-grows)).

How a signal is named is decided too: `<plugin>/<what>`, raised only by the plugin whose
name it carries, through the `sessions` service, which an add-on reaches as well
([What an extension can reach](design.md#what-an-extension-can-reach)).

*Direction.* What is left is its shape, before extensions start raising signals: what the
call hands back to the add-on once it has said its result will come with a signal, and what
the grant is scoped by (the part of the name after the plugin's is the natural resource).
The tool that asks the person
([D10](#d10-what-form-does-a-tools-question-to-the-person-take)) is its first user.

### E8. How does a model call cross the boundary?

Text in, a string out today (`wit/corduroy.wit:300-318`). What it carries is decided
([What the core is made of](design.md#what-the-core-is-made-of)): the core's one request
without tools, a reply with a message, usage and the reason the model stopped, models named
`<provider>/<model>`, and a way to list the models a grant reaches with their descriptions.
The host already fills in the core's request from the text it is given, names models that
way and records the usage and the reason; what an add-on is handed back is still only the
text.

*Direction.* What is left is the form. The request, the reply and the description all grow,
and a record in the world does not, so they are JSON text in the shape a `message` event
holds (which an add-on already reads through `log-read`) unless
[E13](#e13-when-is-a-value-json-text-and-when-a-typed-record) says otherwise. Listing models
is a function of `llm`: it shows only what the grant already reaches
([How the plugin world grows](design.md#how-the-plugin-world-grows)).

### E10. Who bounds a kernel cell's time?

The guest passes any timeout it likes to `kernel.exec`; the host adds no bound, unlike `llm`
and `http`.

*Direction.* A host limit, and decide whether the parameter stays.

### E11. Should every host call be able to fail?

`tools.list`, `plugins.loaded` and `session.stopping` return plain values; they can never
report a refusal.

*Direction.* Decide whether that is deliberate before the signatures freeze.

### E12. Can an add-on ever contribute a loop step, a model or a kernel?

Today no: these are native. It will not be by an export: the exports are closed at six for
all of 1.x, and what the host newly asks of an add-on goes through `handle-call`
([How the plugin world grows](design.md#how-the-plugin-world-grows)).

[What it is for](design.md#what-it-is-for) rules out "never" for the loop: an add-on has to
be able to say what a run's steps are, so a workflow (a reviewer, two agents taking turns)
is plugged in without a rebuild.

[What the core is made of](design.md#what-the-core-is-made-of) rules out one way: an add-on
does not run a loop of its own by asking a model with tools through `llm`. Whatever acts is
a session the harness runs, so an add-on's loop is something it tells the harness, not
something it carries out.

[What an extension can reach](design.md#what-an-extension-can-reach) says where such a loop
is used: a session is started with the name of its loop, so whatever an add-on defines has
to be a name in `Loops` that a session can be started with. It also draws the line a loop
from an add-on has to respect: only a loop acts, through the run's parts, and nobody
outside the harness writes a tool call's trail.

*Direction.* Choose between the two forms left: a service the harness asks at each step, or
a description of a loop that a native step carries out. Either way it is registered under a
name by native code on the add-on's behalf. Models and kernels can still be "never"; one
that does come is a service an extension registers in the core's registry.

### E13. When is a value JSON text and when a typed record?

Gate requests, service payloads and configuration are JSON text; events and tool specs are
records.

Two decisions lean on the answer. A record gains no field and a variant no case within 1.x,
and whatever the host newly asks of an add-on is JSON through `handle-call`
([How the plugin world grows](design.md#how-the-plugin-world-grows)).

*Direction.* State the rule (JSON where the shape is the plugin's or grows; records where
the host must read it).

### E14. Can a running tool report progress?

It can learn it is stopping; it cannot say how far it has got.

[What the record promises](design.md#what-the-record-promises) decides where it goes: to a
live channel in the loop and, at a coarse grain, into checkpoints in the log, reported
through the call scope. A native tool reaches the call scope; an add-on does not.

How an add-on reaches it is decided: reporting is something new, which no grant of
`session` covers, so it is an interface of its own, in the 1.0 world
([How the plugin world grows](design.md#how-the-plugin-world-grows)). The shipped `bash` is
an add-on, and a long command is the case that wants it most.

*Direction.* What is left is its shape, which waits on the live channel and checkpoints
being tried against a real provider: what a report holds, how often one may be made, and
what the grant is scoped by.

### E15. What trail does a tool call made by an add-on leave?

A call through `tools.call` is gated with no session and leaves no `tool/start`
(`crates/corduroy-wasm/src/plugin/host.rs:439`), unlike a call the model makes.

The session is no longer optional here. A session is to hold a `tools` grant of its own,
and a call an add-on makes for it has to pass that grant as well as the add-on's
([What an extension can reach](design.md#what-an-extension-can-reach)).

*Direction.* The same trail as any call, with the session from the call scope, checked
against that session's `tools` grant.

### E16. Are loads by a plugin durable?

A plugin loaded by a plugin is gone after a restart unless its loader loads it again.

*Direction.* Decide whether the host owes this (a plugin store), since `plugins.load`'s
meaning freezes with the interface.

### E18. Which service names are the harness's?

What the host newly asks of an add-on goes through `handle-call`, under a service name the
harness defines, and a prefix is to be set aside for such names, so that a later one cannot
be a name an add-on already answers to
([How the plugin world grows](design.md#how-the-plugin-world-grows)). Today the names are
bare: the loop asks for `compaction`, the harness provides `sessions`, and an add-on may
list any name in `provides`. `sessions` is about to grow: raising a signal, starting a
session and sending one a message from anywhere
([What an extension can reach](design.md#what-an-extension-can-reach)), so its name and its
methods' payloads are the first to settle.

*Direction.* Choose the prefix before 1.0, since the names in `provides` are inside built
add-ons. Decide whether `compaction` and `sessions` move under it, what happens to an
add-on that provides a name under the prefix which the harness has not defined, and where
the one list of defined names and their payloads is kept (with the topics,
[C11](#c11-which-topics-and-payload-fields-are-frozen-and-where-is-the-one-list)).

## F. The WebAssembly host

### F1. Can the host's limits and errors grow?

`Limits` has public fields and no constructor (`crates/corduroy-wasm/src/runtime.rs:19`),
`WasmError` is closed, and `CAPABILITIES` is typed `[&str; 16]`, so the next capability
changes its type.

*Direction.* Growable `Limits` and `WasmError`; a slice for `CAPABILITIES`.

### F2. Which resources are capped?

Linear memory and fuel. Tables, the number of memories and instances are not set
(`crates/corduroy-wasm/src/plugin/instance.rs:52`).

*Direction.* Decide the full list, since each is a field on `Limits`.

### F3. Does the restart count ever reset?

A plugin is restarted up to `Limits::restarts` times, counted from when it was applied.

*Direction.* Decide whether a long-lived agent forgives old traps.

### F4. Does the plugin tree know a plugin has stopped?

Design: "still to do: the same limit in the plugin tree itself". A stopped add-on is still
`Active` in `App`.

*Direction.* Decide whether this needs a new `FiberState` and a way for a plugin to report
it, both of which are the core's.

### F5. Does a protected path reach plugins already loaded?

`WasmRuntime::protect` is a by-value builder: a plugin loaded from an earlier copy does not
see a later protection.

*Direction.* Share the list, or document the order.

### F6. How long does a session's instance of an add-on live?

An add-on is to have an instance for each session that calls it, and one for what belongs
to no session, with no memory shared between them
([What an extension can reach](design.md#what-an-extension-can-reach)). Today there is one
instance, and the restart limit, the queue of events and `activate` all assume it.

*Direction.* Decide when a session's instance is made and let go (first call, the run's
end, a while unused), since a JavaScript or Python instance holds 9 to 13 MB; which
instance hears an event of a session nobody has called the add-on for yet; and whether
restarts are counted for each instance or for the add-on
([F3](#f3-does-the-restart-count-ever-reset)).

## G. The loop and the extension tier

### G5. Which harness types are in the stable set?

[What is stable](design.md#what-is-stable) names `Operation`, `Operations`, `Loop`, `Loops`,
`Provider`, `Providers`. An embedder also depends on `Harness`, `Run`, `Journal`, `Session`,
`Status` and `Outcome`.

[What an extension can reach](design.md#what-an-extension-can-reach) changes who uses
which. The journal is the host's: a loop or an operation reads a session's snapshot and
does not write on a journal. `Operation::run` takes a context; `Run` gains a single tool
call and no longer hands out the `Harness`; and extensions get a handle on sessions (start,
signal, message, list, read) and the host a driver.

*Direction.* List them, with the new ones, by who may use each: an extension, or only a
program that embeds the harness. Make the ones that will grow growable: `HarnessError`
(`src/harness.rs:55`), `Outcome` (`:185`), `Answer`, `SessionInfo`, `Ask`, `Wait`
(`src/session.rs`), the fields of `Status::Waiting`.

### G6. What can a person answer to an ask?

`Answer` is closed. "Always allow this tool" has no form.

*Direction.* Decide with [D1](#d1-is-a-verdict-allow-ask-or-deny-for-good).

### G7. Is `anyhow` part of the contract?

`Operation::run` and `Journal`'s methods return `anyhow::Result`.

*Direction.* Accept it and say so, or use the harness's own error.

### G8. May the API differ by feature?

`Harness::app()` returns a standard-library guard; `kernels()` and provider `serving()` are
feature-gated.

*Direction.* Decide what an extension that builds for both a desktop and a device may use.

### G9. Does a provider stream?

`Provider::complete` returns a whole reply (`src/provider.rs:171`). The `streaming` key of a
provider entry only changes how the provider's own client reads the response; nothing
reaches the terminal until the message is committed.

Decided in [What the record promises](design.md#what-the-record-promises): a streaming
method with a default on `Provider`, its pieces going to a live channel in the loop and, at
a coarse grain, into checkpoints in the log.

*Direction.* What is left is the shape, to be tried against a real provider: what a piece
is (text, thinking, a tool call being written), how a display subscribes to the channel
and for which session, and what a checkpoint's payload holds.

### G10. Why did the model stop, and why did it fail?

`ModelReply` says why the model stopped, when its provider does (`StopReason`: finished,
cut off at its limit, asked for a tool, refused; the set can grow), and the reason is
recorded. Nothing acts on it: a reply cut off at its token limit still ends the run as
finished. `ProviderError` is text, so a loop cannot tell a full context from a rate limit
or a bad key.

*Direction.* What is left is whether those four are the reasons, what a run does with a
reply that was cut off or refused, and a kind on the error.

### G11. What can be set per request?

`ModelRequest` carries messages, tools and a system prompt. The token limit, temperature,
thinking, tool choice and a required output shape are fixed per provider entry.

*Direction.* Decide which belong on the request (it is growable) and which stay
configuration. The request is the core's now, and a plugin's model call fills in the same
one ([What the core is made of](design.md#what-the-core-is-made-of)), so a setting put
there is one an add-on can use too.

### G13. Can a person send more than text?

`run_in` takes `&str` (`src/harness.rs:611`). `Message` and the provider adapter already
carry images both ways; nothing puts one into a user's message.

*Direction.* A way to run a `Message`, and a decision on how the command accepts a file.

### G14. Can a long turn be compacted?

Only what came before the current turn can be summarised (`src/session.rs:1109`). One long
run from one prompt never compacts.

*Direction.* Decide whether the cut may fall inside a turn, which constrains what a
`compaction` event may hide.

### G15. Does the harness bound a tool call?

It awaits a tool with no limit of its own (`src/tools.rs:67`); each tool is trusted to
watch for a stop.

*Direction.* Decide whether a per-call limit is the harness's job or each tool's.

### G16. How deep may child sessions nest?

At most three deep (`MAX_DEPTH`, a constant in `src/children.rs`). That the children of one
message run together is decided
([What an extension can reach](design.md#what-an-extension-can-reach)), and the limit on
depth is one of the two things that bound an add-on granted `sessions`.

*Direction.* Decide whether depth is configuration, a limit a session is started with, or
the constant; and whether a session started from outside, which has no parent, counts from
nothing.

### G17. Where is usage added up?

Per request, as `usage` events, and for a plugin's `llm` call on its `llm/response`.
`Outcome` has no total. That usage carries a cost where the provider states one is decided
([What the core is made of](design.md#what-the-core-is-made-of)) and not built, for want of
the unit.

*Direction.* Decide whether totals are the harness's to keep or a reader's to compute, and
in what unit a cost is written, since it is in the log.

### G19. Who decides what the model sees?

The loop sends the session's projection, which only a `compaction` event changes. Dropping
old tool results, trimming images, marking cache points and adding reminders are other ways
to manage a context, and an extension can do none of them without writing to the log.

*Direction.* Decide whether shaping the request is a contribution of its own (a step
between the projection and the provider) or always means a custom `Loop`.

### G20. What limits a run besides the number of turns?

`max_turns` and the restart limit. There is no budget of tokens, cost or time, for a run, a
session or a child.

Where a limit is written is decided: a session is started with a set of limits that can
grow, turns being the first, counted as its loop counts them
([What an extension can reach](design.md#what-an-extension-can-reach)).

*Direction.* Decide which budgets join that set, and whether each is the loop's own to keep
or policy asked at a decision point before each model request
([D3](#d3-which-decision-points-exist-at-10)).

### G21. Who retries, and who falls back?

Retries are inside the provider's adapter (`crates/corduroy-providers/src/adapter.rs:99`).
A model that still fails ends the run as failed; there is no falling back to another model
or provider.

*Direction.* Decide whether that is the provider's for good, or the loop's (which needs the
error kinds of [G10](#g10-why-did-the-model-stop-and-why-did-it-fail)).

### G22. Is the event stream the only way to observe a run?

A front end, a metrics exporter and a tracer would all be listeners. Events carry a time
and no duration, and a listener runs on the thread that committed
([C4](#c4-do-listeners-run-on-the-committing-thread)).

*Direction.* Confirm it, and check that a trace exporter can be written as an extension
from what events hold today.

## H. The standard set, the command and the agent file

### H1. Which provider kinds and keys are contract?

`[providers.*]` is the one section not built from a kind of the `Standard`
(`crates/corduroy-cli/src/agent.rs:645`). Its keys follow oi-kit's settings, and "other
providers oi-kit knows can be named directly as the `kind`": the agent file, a stable
contract, inherits whatever an alpha dependency accepts.

[What the core is made of](design.md#what-the-core-is-made-of) changed two things here. The
`serve` key is gone, since the `llm` grant is the one list of models an add-on may ask, by
`<provider>/<model>`. And a model's description (what it takes, its context) is stated or
corrected in the file, under `[providers.<name>.models.<model>]`; those keys' names are
among the ones to confirm.

[What is safe by default](design.md#what-is-safe-by-default) changes a third: every
credential comes through `Secrets`. A provider's `api_key_env`, and an MCP server's
`bearer_token_env`, each read the environment on their own today, and give way to naming a
secret.

*Direction.* Provider kinds in `Standard` like the rest, and a named list of kinds and keys
that are ours, those that describe a model and the one that names its secret among them.

### H2. Does 1.0 ship on oi-kit?

`corduroy-providers` is pinned to an exact alpha. It is confined to one crate behind
`Provider`.

*Direction.* Decide whether that is acceptable for 1.0 or whether clients of our own for
two or three APIs come first.

### H3. What of the harness can the agent file set?

Provider, model, system prompt and turn limit. Not the loop (so a `Loop` an extension
registers can never be chosen), the restart limit, extra steps, or the WebAssembly limits.

The file is where the person says what a session they start is started with: its model,
loop, instructions and limits, holding everything the file allows
([What an extension can reach](design.md#what-an-extension-can-reach)). So the loop has to
become a key, and the turn limit one entry of a set. How many sessions the driver runs side
by side is a setting with nowhere to be written yet.

*Direction.* Add the loop and the number of sessions run at once. Decide which of the rest
become keys and which are a `Standard` part, and which of them the command line may also
say for a new session.

### H5. Can the `Standard`'s own types grow?

`Asked` and `Places` (`crates/corduroy-std/src/`), `Transport` (`corduroy-mcp`),
`ProviderSpec` and `Thinking` (`corduroy-providers`) are built by literal.

*Direction.* Growable, with constructors.

### H6. Where does an installed command find the shipped add-ons?

`Places::new` ends the plugin path with the source tree the command was built from
(`crates/corduroy-std/src/places.rs:39`), and begins it with the agent file's directory. A
shipped add-on's name is to resolve to the shipped one before anything a workspace holds
([What is safe by default](design.md#what-is-safe-by-default)), so the order changes with
the layout.

*Direction.* Decide the installed layout (beside the binary, a data directory, embedded),
and with it where the account-level place is on each platform and what the agent file's
strings call it, beside `{workspace}`, `{data}` and `{home}`.

### H7. Are the agent file's older names final?

`[[plugins]]` reaches add-ons only, and the names stay by an earlier decision. 1.0 is the
last moment to change that.

*Direction.* Confirm.

### H8. How much of MCP is in 1.0?

Tools, listed once at start. No resources, prompts, sampling, list changes or reconnection.

*Direction.* Decide the scope; all of it is addable in the extension.

### H10. Which platforms does 1.0 promise?

CI runs the whole suite on Linux; on Windows and macOS it builds the command and tests the
core and the PowerShell kernel.

A confined shell is promised on Linux and macOS
([What is safe by default](design.md#what-is-safe-by-default)), and each guarantee has a
test, so those tests run on both. What confines a shell on Windows has not been looked
into; until one exists the shell there is opt-in, and the file tools work as everywhere.

*Direction.* Decide whether the full suite on all three is a condition of 1.0. Find out
what Windows offers for confining a process, and then either promise a confined shell
there or say that 1.0's shell on Windows is the plain one, chosen by the person.

### H11. Is there a front end other than a Rust program?

Driving a harness means linking it. An editor, a web page or a program in another language
needs a protocol over a socket or a pipe. The design calls such things front ends that
"need nothing from the core beyond sessions and signals".

Part of what a server needs is decided
([What an extension can reach](design.md#what-an-extension-can-reach)): the command gets a
mode that stays up with no terminal and takes on whatever arrives, and the host's driver
runs every session that is ready. That mode serves plugins and speaks no protocol of its
own. A front end's notice of work in the background (a line, a toast) is the front end's.

*Direction.* Decide whether 1.0 ships a server front end, and which protocol it speaks. If
it does, the events and the harness's verbs become a wire format that freezes like the log.

### H14. What is in the standard set at 1.0?

Shell kernels, the HTTP client, secrets from the environment, MCP, the providers and the
file log. Not a sandboxed kernel, a scheduler, a keychain or a second store.

The confined kernel ships, with its filter for hosts and masked credentials, and is the
default agent's shell ([What is safe by default](design.md#what-is-safe-by-default)).

*Direction.* Fix the rest of the list. A second store is built as a proof
([What the freeze waits on](design.md#what-the-freeze-waits-on)); decide whether it then
ships. A scheduler and a keychain are stated to be out, or added.

### H15. Which outside conventions does 1.0 commit to?

MCP, the hint files (`AGENTS.md`, `.goosehints`, `CLAUDE.md`), skills as folders, and APIs
in OpenAI's and Anthropic's shapes. Each moves on its own schedule.

*Direction.* Name the ones that are part of the promise, and the version of each.

## I. Add-ons and the guest SDKs

Add-ons may keep changing after 1.0. These are the few things about them that others will
depend on.

### I1. Is `corduroy-guest`'s API stable?

Every Rust add-on builds on it. It is neither in the stable list nor declared free to move.
What is promised is the built add-on, not its source
([How the plugin world grows](design.md#how-the-plugin-world-grows)): whether source
written against one release of the crate builds against the next is this question.

*Direction.* Decide which, and version it against the WIT package.

### I2. How do the SDKs find the world, and how are they versioned?

Both read `wit/corduroy.wit` from a path relative to this repository, which a published
package would not have, and their versions are independent of the WIT package's.

*Direction.* Embed the world in each SDK and tie the versions.

### I4. What is an add-on, as something shipped?

A bare `.wasm` file, and an entry someone writes in the agent file. No name, version,
description or wanted grants travel with it
([D6](#d6-does-a-plugin-ship-a-statement-of-what-it-wants)); nothing is signed; there is
nowhere to fetch one from.

*Direction.* Decide whether 1.0 defines a package (a component and a manifest), even if a
registry comes later. The hash already asked of the gates at `plugin/load` is the hook.

### I5. Which guest languages are promised?

Rust is the reference; JavaScript, TypeScript and Python have SDKs; the script host runs
scripts with nothing compiled. A JavaScript or Python add-on is 12 to 19 MB built and
holds 9 to 13 MB for each instance.

The script host is promised already: what a script is handed (the `host` object) and the
plugin object it returns hold for all of 1.x
([How the plugin world grows](design.md#how-the-plugin-world-grows)).

*Direction.* Name the first-class ones among the compiled languages. Write the script
host's `host` object down as a reference page before 1.0, since it is now a contract and
is described only in `sdk/js/script-host/host.js`.

### I6. What does "the agent extends itself" need at 1.0?

1.0 claims it, and it is the first thing to be shown
([What it is for](design.md#what-it-is-for)). The pieces exist (`plugin-manager`, off by
default; the script host; the `tools` capability). Three things it leans on do not: loads
that survive a restart ([E16](#e16-are-loads-by-a-plugin-durable)), a person approving a
load ([C15](#c15-can-a-person-answer-an-ask-at-pluginload)), and a scheduler.

The scheduler waits: it is an extension that raises signals and starts sessions, the way
for it to do so is decided, and it is not in 1.0
([What an extension can reach](design.md#what-an-extension-can-reach)). An automation the
agent wrote can start a run when an event fires, holding at most its own grants; what it
cannot do until there is a scheduler is wake at a time.

*Direction.* The first two are conditions of the freeze: something the agent built must
still be there tomorrow, and the person must be the one who allowed it.
