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

Where one form of an answer has been built to be tried, the entry says so in a paragraph
marked *Prototype*, and [Prototypes](prototypes.md) says what was built and what it found.
A prototype decides nothing: the question stays here until it is answered.

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

## What is decided

The questions were not equally weighty. Eight groups of them set the direction, and all
eight are decided. What is left is detail that hangs off one of those answers, or a
mechanical choice (which types may grow, which names are final), best made in one sitting
per section. Every question below is read against the eight.

Why this exists, and the two uses 1.0 is measured against, are in
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
[What an extension can reach](design.md#what-an-extension-can-reach). What a person reaches
the agent through, what the standard set holds, where a credential comes from, which parts
of a provider's entry are ours, and what an add-on is as something kept and passed on are
in [How people meet it](design.md#how-people-meet-it).

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

### P10. Who writes add-ons: people or the agent?

The design says the purpose of the plugin system is that the agent adapts the harness to its
task. The SDKs, tutorials and reference are written for people. The two authors want
different things: a model wants a script it can write in one go, a tight error when a grant
refuses it, and a person to approve the load; a person wants packaging, versions and
somewhere to publish.

The author is named ([How people meet it](design.md#how-people-meet-it)): the agent writes,
and the person reads and says yes. So a package is a folder of text around a script or a
component, with no registry and a version that decides nothing.

*Direction.* What is left is the order of work: whether the script host and the package are
finished before the SDKs' own packaging, and how much the tutorials turn towards reading
and approving what an agent wrote.

### P14. How does it reach people?

A new terminal agent asks someone to change a habit. The alternatives are to sit behind
something they already use (an editor, through a protocol editors and agents share), or
inside something someone else ships. The mechanism is decided
([How people meet it](design.md#how-people-meet-it)): the terminal at 1.0, then a page in a
browser served by an extension, which a desktop shell or a phone can show. This asks about
the intent.

*Direction.* The purpose is a harness a person uses, with the command line as its front end
and adoption not a goal of 1.0 ([What it is for](design.md#what-it-is-for)). Leave this
until there is someone other than its author to reach.

### P15. What will show that it is worth having?

Nothing outside this repository depends on it yet. Before the core is frozen for good,
something should show the purpose is real.

One thing is decided ([What it is for](design.md#what-it-is-for)): the test-driven pair is
a proof that the surface is enough, and not the demonstration. The demonstration has to
show growth, which is the thesis: what the agent builds takes the place of model calls it
used to make.

The candidate, not yet chosen: a piece of work begun by a small model on this machine and
carried to where the model is no longer needed. The agent builds the tools and automations
the work takes until what is left runs as code, with no model call, or with none but a
small one that decides between a few things. Like a developer hired once to build
something that then runs without them. It would show that what was built stays and works
when the model is taken away. It is still one case, and closer to the thesis than the
pair.

*Direction.* Choose the piece of work, before 1.0, and what is measured: model calls and
tokens for the same task at the start and at the end, and whether the end state answers
correctly with the model switched off. Say whether it is a condition of 1.0 or what 1.0 is
shown with. It leans on what [I6](#i6-what-does-the-agent-extends-itself-need-at-10) lists
as not built: what the agent made has to be there the next day, and something has to start
it without a model asking.

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
can hold the list of places marked for growth, so that an addition anywhere else fails. The
trusted base is kept to a budget ([What is safe by default](design.md#what-is-safe-by-default)):
set its figures (lines, dependencies, the WebAssembly engine counted with them) and say
which check fails when one is passed. For
the add-on fixtures, decide which guest languages are kept and where: a JavaScript or
Python add-on is 12 to 19 MB, which is a lot to hold in the repository for every release.

*Prototype.* [X4](prototypes.md#x4-the-trusted-base-measured): a script that measures the
trusted base (10 713 lines; 19 crates from outside without the WebAssembly engine and 110
with it) and compares it with a budget file. It only measures: the file holds the day's
figures, and nothing fails on it.

### A5. What carries the version 1.0, and are the versions tied?

The crates are `0.1.0`, the WIT package is `corduroy:plugin@0.1.0`, the log's schema is `2`.
Whether these move together decides what a guest SDK, a store and an extension each pin.
What a version of the world means is decided, and an add-on built against its 1.0 loads on
every host whose world is 1.x
([How the plugin world grows](design.md#how-the-plugin-world-grows)).

The world is ours alone at least until 1.0, and no other host implements it
([How the plugin world grows](design.md#how-the-plugin-world-grows)), so nothing outside
this repository has to follow its version yet.

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
status. Every kernel starts from an empty environment
([What is safe by default](design.md#what-is-safe-by-default)), so the environment a `Spawn`
names, with the few plain variables the kernel gives every process, is all a process gets.

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

What it means is decided ([What is safe by default](design.md#what-is-safe-by-default)): a
secret names the hosts it may go to, in the core before the freeze; one that names none
goes nowhere; a secret in the keychain has its hosts beside its value, said by the person
when they put it in, and one from the environment or a file says them in the agent file;
and a host a secret does not name is offered to the person, never taken.

*Direction.* What is left is the form. A defaulted `hosts()` on `Secret`, checked wherever
a value is written into a request: the host for an add-on, a kernel's filter for a shell, a
provider for its key. Then how hosts are written: exact names only, or a
[pattern](#b12-what-does-a-pattern-mean-for-good); the key by which a `[secrets.*]` entry
lists them; how `corduroy secrets set` takes them and `corduroy secrets` shows them; and
what becomes of an item put in before it had any, which now goes nowhere until the person
says where.

*Prototype.* [X1](prototypes.md#x1-a-secret-names-the-hosts-it-may-go-to): `Secret::hosts`
as a scope over host names, checked for an add-on's request, a provider's key and an MCP
server's token; `hosts` in an environment secret's entry; `corduroy secrets set --hosts`,
kept beside a keychain value. A secret that says nothing is still sent as before, so the
part of the decision that stops it is not on yet. It found, among other things, that
`hosts()` cannot fail where a keychain can, and that hosts kept as a second item are a
second read of the keychain.

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
by refusing them now. A `service` grant is to be `service:method`
([What an extension can reach](design.md#what-an-extension-can-reach)), so say with it what
`:` means in a service's or a method's name (refused in both is the simplest), and that
`sessions:*` and a bare `sessions` are the same grant.

Where the code stands: `service:method` grants are built (`Scope::permits_part`), and
`sessions`, `sessions:*` and `*` are the same grant, by a test. A `:` in a name is not
refused anywhere yet. A secret's hosts, as [prototyped](prototypes.md#x1-a-secret-names-the-hosts-it-may-go-to),
are patterns of the same syntax, where `*` crossing a `.` matters more than it does for a
path.

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

*Prototype.* [X6](prototypes.md#x6-a-calls-scope-carried-to-another-thread): `call::carry`
and `Carried::enter`, an addition to the core's `call`. It found that a wait said on the
other thread does not reach the call unless the tool says it again, and that nothing makes
a tool carry its scope.

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

### B25. Whose job is a tool's input?

Neither the core nor the loop checks a call's input against the tool's schema. Each tool
does, or does not. The size of a result is decided
([Keeping within what the model holds](design.md#keeping-within-what-the-model-holds)): the
tool says it, and the harness holds each call and each step to a share of the model's
context.

*Direction.* Say it is the tool's, or have the loop refuse an input that does not fit the
schema before the gates are asked.

### B26. How are tool names kept apart?

There is one flat set of names. An MCP server's tools are `<server>__<tool>`; an add-on's
are bare, and the second to register a name is refused.

How one tool replaces another is decided, and built
([What an extension can reach](design.md#what-an-extension-can-reach)): where the agent
file says so, and only by a tool with the same input schema and the same word on being
repeated. Two things it showed are left. What a tool returns is not compared, because no
tool declares the shape of its output: `lean-shell` and `pi-tools` agree on the form of
`bash`'s result only because they share its code. And the loser of a clash that was not
said to be a replacement does not start at all, its other tools with it, by the order of
the agent file.

*Direction.* A naming rule before third-party add-ons exist, and a decision on whether an
add-on that lost one name should keep its others. Then whether a tool may declare its
output's shape. The place for it is there already: a named value in the tail of
`tool-spec` ([E2](#e2-can-what-a-guest-returns-grow)) and a builder method on the core's
`ToolSpec`, so it can be added after 1.0. What is not settled is what it would mean: whether
the harness holds a result to it, and what happens to one that does not fit; whether the
model is told it; or whether it is only what a replacement is compared by. A tool like
`bash` or `read` returns free text and would have nothing to declare.

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
emits five topics of its own. Nothing but a person kept them in step, and the reference
lacked `plugin/suspended` and `plugin/dropped` until a check was tried.

[What an extension can reach](design.md#what-an-extension-can-reach) decides several things
that are in the log and have no shape yet: what `session/start` holds (a model, a loop,
instructions, grants, limits, and who started the session), a message that waits for a run
to end, a signal kept for a wait not yet recorded, and an answer to an ask about a session.
A message that waits cannot be a `message` event until its run begins, since a run is read
as starting at the last message a person wrote.

*Direction.* One list, checked by a test, marked with which topics are contract and which
are diagnostics. Shape the new ones as they are built, under the rule that old logs still
read: a `session/start` that says nothing runs as the agent is configured.

*Prototype.* [X3](prototypes.md#x3-one-list-of-topics-checked): a test that holds the
reference to the topics the code raises, read from the sources as text. It found the two
topics named above, which are now in the reference. It checks names only: nothing of a
payload, and nothing of which topics are contract.

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

*Prototype.* [X8](prototypes.md#x8-a-session-that-branches-from-another): a spike against
the store in memory and the JSON Lines file, not the folder store. The log did not change:
a branch is one `session/start` with `parent` and `from`. It found that a branch can begin
only where the conversation is whole, that `parent` then means two things, and that
removing a session has to remove its branches, since they begin with its events.

### C15. Can a person answer an ask at `plugin/load`?

Not yet (design, "still to do"). Asks are answered per tool call (`Harness::answer`).

That it can is decided ([Events, gates and hooks](design.md#events-gates-and-hooks)): an
ask at any declared point waits in the log for a person, and one `gate/answer` names a tool
call, a load or a session's start. One of the six guarantees waits on its being built: that
a person approved what the agent loads
([What is safe by default](design.md#what-is-safe-by-default)).

Two more things are offered to a person while a call runs, a host the shell was refused
and a credential the agent asked for. Neither holds the call: it fails, the person answers,
and the model tries again.

What a yes at `plugin/load` attaches to is decided as well
([How people meet it](design.md#how-people-meet-it)): the grants and not the code, so the
person is asked about what a package newly wants and nothing else. A package that wants
more than its loader holds is asked of the person whatever the gates say, and the answer
is kept at account level, where it also brings the package back after a restart.

*Direction.* What is left is the form: the fields by which `gate/answer` and
`Harness::answer` name a load or a session, with old logs' answers, which name a tool call,
still read; and whether a refusal the person may lift afterwards (the host, the credential)
is an ask of that kind or an event of its own.

One choice runs through all of it: whether the call that led to an ask waits, as a tool
call a gate asked about does, or fails and is made again once the person has answered.
The leaning (28 November 2026) is to try the second first, as the smaller, and to go to
waiting if a revised design allows it, which is what other harnesses do here. It is not
decided.

*Prototype.* [X9](prototypes.md#x9-an-ask-that-reaches-a-person-anywhere): the second form,
for `plugin/load` and `session/start`, where the host says it takes answers. An ask is a
`gate/decision` with an `ask` id, an answer a `gate/answer` naming it, and the same request
asked for again finds its answer. It found that allowing something does nothing until the
model asks again, and that "the same request" has to be defined for each point.

### C18. How many processes may use one log?

One. A log has one writer, and the file store locks the file when it opens it, so two
terminals cannot work in the same workspace at once.

*Direction.* Decide whether one process per log is the contract, with a service that owns
the log and serves the others, or whether readers may share. A front end is to be an
extension inside the one process ([How people meet it](design.md#how-people-meet-it)), so
a page in a browser does not need a second one.
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
call was decided against. Remembering is decided to be the gate's
([Events, gates and hooks](design.md#events-gates-and-hooks)): a lasting answer is the
person's, the gate that asked keeps it, and it needs no verdict of its own.

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

Unloading is not a point today. The agent may remove what it built
([How people meet it](design.md#how-people-meet-it)), and a package it built may guard a
decision point, which no answer of the person's switches off
([What is safe by default](design.md#what-is-safe-by-default)).

*Direction.* Decide whether the loop declares the other two before 1.0, since gates written
by third parties will key on the names. For `session/start`, what is left is the request's
JSON ([D2](#d2-are-the-gate-requests-json-shapes-contract)). Decide whether taking away a
plugin that guards a point is asked first, as loading one is. The leaning is yes, as a
point the gates are asked at; what it might break has not been thought through.

*Prototype.* [X5](prototypes.md#x5-the-decision-point-before-a-session-starts): the harness
declares `session/start` and asks before the `sessions` service starts a child, with
`{"session", "parent", "starter", "task"}`. That is all there is to put in the request
until a session is started with a model, a loop, grants and limits. An ask there reaches
a person in the form [X9](prototypes.md#x9-an-ask-that-reaches-a-person-anywhere) tries,
where the host takes answers, and is a denial otherwise.

### D4. Is name order, first strictest, the final rule?

Gates are asked in name order and the first to give the strictest kind is recorded.

*Direction.* Confirm.

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

`tool-spec` and `plugin-info` can: each ends in a list of named values
([How the plugin world grows](design.md#how-the-plugin-world-grows)). `content` is still
text or an image, and gains no case within 1.x, so whatever is to grow there has to be
given its place before 1.0.

*Direction.* A general typed block (`mime-type`, `data`) beside `image`. Say whether the
core's `ToolSpec` keeps the named values it does not know, so that a gate or another
plugin can read what a tool said that this host has no word for.

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

Where the choice stands (November 2026): undecided, leaning to the description. What speaks
for it is that a person can read a loop before allowing it, that one can be put together
while the agent runs, and that it is the form other workflow engines hold. What is not
known is what it has to be able to say. The case to write it against is a loop that works
until a set of tests passes, starting with a few of them and adding more as they pass:
steps in a line, the line taken again until something is true, and perhaps a loop inside a
loop. What every run keeps (its limits on turns, time and tokens, a stop) is the
harness's whichever form is chosen, so neither form has to bound itself.

*Direction.* Choose between the two forms left: a service the harness asks at each step, or
a description of a loop that a native step carries out. Write the test-driven pair
([What the freeze waits on](design.md#what-the-freeze-waits-on)) as a description first,
and see what it cannot say: every such thing is either a word the description gains, which
is then frozen, or the reason to ask a service instead. A description may also be what a
script returns, which keeps it readable and lets it be assembled. Either way it is
registered under a name by native code on the add-on's behalf. Models and kernels can
still be "never"; one that does come is a service an extension registers in the core's
registry.

Writing the description was not started, and is left open for now: the first thing such a
loop does beyond the standard one, running a check and telling the model what came of it,
has no form yet ([Prototypes](prototypes.md#what-waits-on-a-decision) says it in full). It
waits on how a message says who sent it ([B28](#b28-is-there-one-person)) and on a tool
call a loop can make with its trail.

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

What the host owes is decided ([How people meet it](design.md#how-people-meet-it)): a
package the person approved is loaded again at start, from their answers, and a bare
component loaded through `plugins` lasts as long as its loader, as today.

*Direction.* What is left is the interface, whose meaning freezes: how a plugin asks for a
package to be loaded (a function of `plugins`, inside its grant, or the load that exists
given a folder), and what the call returns while the person has not yet answered.

### E18. Which service names are the harness's?

What the host newly asks of an add-on goes through `handle-call`, under a service name the
harness defines, and a prefix is to be set aside for such names, so that a later one cannot
be a name an add-on already answers to
([How the plugin world grows](design.md#how-the-plugin-world-grows)). Today the names are
bare: the loop asks for `compaction`, the harness provides `sessions`, and an add-on may
list any name in `provides`. `sessions` is about to grow: raising a signal, starting a
session and sending one a message from anywhere
([What an extension can reach](design.md#what-an-extension-can-reach)), so its name and its
methods' payloads are the first to settle. Its methods' names are now written in grants: a
`service` grant names `service:method`, and a bare name every method, which is built.
Today `sessions` has one, `start`.

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
([F3](#f3-does-the-restart-count-ever-reset)). An event that arrives while an add-on is in
a call is to wait in a queue of the add-on's own
([Events, gates and hooks](design.md#events-gates-and-hooks)): say whether that queue is
each instance's, how many events it holds, and what becomes of them when the instance
traps or the add-on is stopped.

Where the code stands: the queue is built for the one instance there is, and so had to
answer each of these one way, none of them confirmed
([Prototypes](prototypes.md#built-from-decisions-and-what-that-left-open)): it holds 1024
events (`Limits::missed_events`) and drops the oldest with a `plugin/missed`; it outlives a
trap, and the instance started in its place hears what was kept; a stopped add-on's is
dropped.

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

`Answer` is closed: allow, or deny with a reason. "Always allow this tool" has no form.

What an answer may be is decided
([Events, gates and hooks](design.md#events-gates-and-hooks)): allow or deny, once or from
now on, for exactly what was asked or within a pattern the person names, so wider or
narrower than the question; a yes carries no words and a no carries a reason; and the gate
that asked remembers a lasting answer.

One thing waits on this. A small model refused a `write` by a person does the same thing
with `bash`, and the second question shows a shell command with nothing to say that it
follows a refusal ([what a small model does](local-model-findings.md)). That a question
follows a refusal of the same target is said by the gate that asks, and not worked out by a
front end, so that the terminal, the page and anything else show it alike. It is built
together with the lasting answers, since a remembered refusal is the same mechanism as a
remembered "always allow", pointing the other way.

*Direction.* What is left is the form, built with
[C15](#c15-can-a-person-answer-an-ask-at-pluginload): `Answer` made growable and given its
cases; how `gate/answer` writes an answer's reach; what a pattern is matched against at
each point (a tool's name, its input, a host, a path) and in which syntax
([B12](#b12-what-does-a-pattern-mean-for-good)); where a gate keeps what it remembers (its
`state`, which is the workspace's log, or with the person's other answers at account
level), and how the person sees and takes back a lasting answer; and the field by which a
gate flags a question that repeats what was refused.

### G7. Is `anyhow` part of the contract?

`Operation::run` and `Journal`'s methods return `anyhow::Result`.

*Direction.* Accept it and say so, or use the harness's own error.

### G8. May the API differ by feature?

`Harness::app()` returns a standard-library guard; `kernels()` and provider `serving()` are
feature-gated.

*Direction.* Decide what an extension that builds for both a desktop and a device may use.

### G9. What of a streamed answer is still to settle?

A provider streams: it tells the pieces of its answer as the model writes them, the loop
shows them to whoever watches a session and checkpoints them in the log, and a request that
never answered leaves what it had written
([What the record promises](design.md#what-the-record-promises)). That was tried against a
streamed response on loopback and not yet against a provider over the network.

*Direction.* Three things are left. Whether a tool call is told as it is written or only
whole: the piece has room for either, and oi-kit gives it whole. Whether the checkpoint
interval is a key of the agent file or stays the host's to set
([H3](#h3-what-of-the-harness-can-the-agent-file-set)). And whether what a model thinks
belongs in a checkpoint at all, or only its length: it is the larger part of what a small
model writes, and a signed one is of no use to a reader.

### G10. Why did the model stop, and why did it fail?

`ModelReply` says why the model stopped, when its provider does (`StopReason`: finished,
cut off at its limit, asked for a tool, refused; the set can grow), and the reason is
recorded. A session says how the model's last answer stopped (`Session::stopped`), and
the terminal tells the person when it was cut off or declined. A run whose answer was cut off
ends as `cut-off`, not `finished`; beyond that the loop acts on none of it: what to do (go
on after a cut-off answer, ask the person after a refusal) is policy, for a step of the
loop an extension adds. `ProviderError` is text, so a loop cannot tell a full
context from a rate limit or a bad key.

*Direction.* What is left is whether those four are the reasons, and a kind on the error.

### G11. What can be set per request?

`ModelRequest` carries messages, tools and a system prompt. The token limit, temperature,
thinking, tool choice and a required output shape are fixed per provider entry.

The provider entry's other keys are settled
([How people meet it](design.md#how-people-meet-it)). `max_tokens`, `temperature` and
`thinking` stay as written until this is answered, and are the last of that entry's keys to
confirm.

*Direction.* Decide which belong on the request (it is growable) and which stay
configuration. The request is the core's now, and a plugin's model call fills in the same
one ([What the core is made of](design.md#what-the-core-is-made-of)), so a setting put
there is one an add-on can use too.

### G13. Can a person send more than text?

`run_in` takes `&str` (`src/harness.rs:611`). `Message` and the provider adapter already
carry images both ways; nothing puts one into a user's message.

*Direction.* A way to run a `Message`, and a decision on how the command accepts a file.

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

*Prototype.* [X7](prototypes.md#x7-what-a-run-used-on-its-end): a `turn/end` holds `used`,
the run's own requests added up, beside the report's sum
([T8](trials.md#t8-who-adds-up-what-a-run-used) offers both). The two differ where a run
started sub-agents, and neither counts an add-on's own model calls, so a total has to say
which requests it counts before it says anything.

### G19. Who decides what the model sees?

The loop sends the session's projection, which only a `compaction` event changes. Dropping
old tool results, trimming images, marking cache points and adding reminders are other ways
to manage a context, and an extension can do none of them without writing to the log.

That shaping the request is a step of its own is decided
([Keeping within what the model holds](design.md#keeping-within-what-the-model-holds)):
extensions register one as they register an `Operation`, it changes what is sent and not
the record, and it is not a ninth kind of contribution.

What the model was shown can be read back only in part: a request names its system prompt
and its tool declarations by hash, and the log holds each once
([What the record promises](design.md#what-the-record-promises)). The messages it carried
are taken to be the projection, which stops being true once a step may change them.

*Direction.* What is left is the form, before extensions write such steps: the registry
and the trait, and what a step is handed (the core's `ModelRequest`, and what of the
session); the order several run in, name order as for prompt parts and gates or an order
they state ([B10](#b10-in-what-order-are-prompt-parts-rendered)); whether a step may change
the system prompt and the tools or only the messages; what is recorded when a request was
shaped (which steps ran, and a hash of what was sent, at the least), so that review does
not take the projection for what the model saw; and whether an add-on can be one, through
`handle-call` under a name the harness defines
([E18](#e18-which-service-names-are-the-harnesss)), or only an extension.

*Prototype.* [X2](prototypes.md#x2-shaping-a-request): a `Shapers` registry in the harness,
a `Shape` handed the session and the core's `ModelRequest`, run in name order by the
standard loop, free to change the messages, the system prompt and the tools and not the
model, with `shaped` and `messages_hash` on the `model/request` of a request something
changed. An add-on cannot be one. It found that the record says a request was shaped and
not how, and that a loop which does not ask for shaping gets none.

### G20. What limits a run besides the number of turns?

Decided for the agent's own runs
([What an extension can reach](design.md#what-an-extension-can-reach)): turns, the time a
run is driven for and the tokens the model writes in it, the last two kept by the harness
whichever loop runs, with cost to join them. They are settings of the harness today
(`max_turns`, `max_seconds`, `max_output_tokens` in the agent file), the same for every
session.

*Direction.* What is left waits on sessions being started with limits of their own: how a
child's budgets relate to its parent's (a share of what is left, or its own), whether what
a child writes counts against the run that waits for it (today a child shares its parent's
clock and has its own count of tokens), and the unit a budget of cost is written in
([G17](#g17-where-is-usage-added-up)).

### G21. Who retries, and who falls back?

Retries are the provider's, made by one layer and each recorded with who made it
(`model/retry`, `by`): see
[What the record promises](design.md#what-the-record-promises). A model that still fails
ends the run as failed; there is no falling back to another model or provider.

*Direction.* Decide whether falling back is the provider's for good, or the loop's, which
now has the error kinds to go by and would say `by` its own name. The client beneath the
provider is heard, not told: ask oi-kit for a way to switch its retries off, so that
hearing a log line is not what one layer of retries rests on.

### G22. Is the event stream the only way to observe a run?

A front end, a metrics exporter and a tracer would all be listeners. Events carry a time
and no duration, and a listener runs on the thread that committed, which is decided to
stay so ([Events, gates and hooks](design.md#events-gates-and-hooks)).

*Direction.* Confirm it, and check that a trace exporter can be written as an extension
from what events hold today.

### G23. Can an extension be written in a language other than Rust?

An extension is Rust compiled into the command. Other languages reach the agent as add-ons
(JavaScript and Python, sandboxed) or as MCP servers (tools only), and neither is given what
an extension is: a kernel, a decision point, a step of the loop, the host's authority. A C
interface for extensions would open that tier to anything that can call or be called
through C. Nothing is built, and nothing has been tried; this is to be studied on its own
before any of it is decided.

Where the code stands. The core's types cannot be given a C layout as they are: `String`,
`Vec`, `Result`, `Arc<dyn Tool>` and a borrowed `Context` have none. So the interface would
be a mirror of its own (opaque handles, strings as a pointer and a length, tables of
function pointers) in a crate outside the core, an extension like any other, which
implements `Plugin` and hands the foreign code a handle to its `Context`, so that every
registration is still an effect and unloading reverts it. The core gains no dependency and
stays `no_std`. Most of what an extension contributes already crosses as text: `Tool::call`
takes JSON, `Service::call` a caller, a method and a payload, and gates, prompt parts,
secrets and listeners have the same shape. Three things do not: `Provider`, `Operation` and
`Loop` return futures, which over C become calls that block; `Kernel` and `Process` hold
pipes and state that lasts; and the traits are `Send + Sync`, which a C author can only
promise. The workspace forbids `unsafe`, so the crate would be its one documented
exception, as `corduroy-guest`'s bindings are among the add-ons.

What has to be decided:

1. *Whether it is wanted in the process at all.* A protocol to an extension in a process
   of its own, as MCP is for tools, needs no `unsafe` and outlives the extension's crash,
   and costs speed and a wire format to design and keep. In the process, a fault in foreign
   code takes the command down, where a trap in an add-on ends one instance, and a panic
   has to be caught at the boundary in both directions.
2. *Loaded when the command runs, or linked when it is built.* A library named in the
   agent file is the more extendable, and ends the rule that an extension needs the command
   rebuilt ([Three tiers](design.md#three-tiers-core-extensions-add-ons)): a path would
   carry the host's whole authority, so a workspace's agent file must never be able to name
   one, which waits on a guarantee that is decided and not built
   ([What is safe by default](design.md#what-is-safe-by-default), the fifth). A static
   library and a header keep the rule and need no loader, and each author builds a command
   of their own.
3. *Whether the interface freezes with 1.0.* It would be a fourth contract beside the
   world, the core's traits and the log ([What is stable](design.md#what-is-stable)), and
   one that cannot change by adding unless it was laid out for it from the start.
4. *How much of the tier it covers.* The contributions that cross as text only, or the
   async traits and kernels as well, and in which order.
5. *Whether the other direction is the same work.* A program in another language that
   embeds the harness and drives runs needs a C interface to `Harness`, which is a
   different surface from the one an extension is handed.

A prototype exists to study it with: `crates/corduroy-cabi`, a header, an extension in C
and tests that run it, covering tools, services, gates, prompt parts, secrets, listeners,
effects and a model provider that answers through a completion, linked in or loaded from
a shared library. It decides nothing, ships
in nothing, and [what it found](c-extensions.md) adds to the list:

6. *Who vouches for a loaded library.* The prototype does both: linked, the `unsafe`
   declaration of the entry function is written by whoever builds the command; loaded,
   it is found inside the one crate, but loading is still `unsafe` to call, since a wrong
   file corrupts the process. A command that forbids `unsafe` can load an extension only
   if the crate offers a safe function and so vouches for a file it has not seen. And a
   loaded library is never unloaded, so unplugging it reverts what it registered and
   leaves its code in the process.
7. *Whether a call that blocks is enough.* The aim is systems languages and the
   languages of a platform (C, C++, Zig, Swift, Go, C#), for code against a platform's
   own interfaces, and not scripting languages, which stay add-ons. For those a blocking
   call is enough. What is untried is a platform interface that must be called on one
   thread or answers through its own callback, which may want tools answered through a
   completion, as the prototype's provider is.
8. *Which misuse the host refuses.* A context kept after `apply`, a handle after `drop`, a
   completion completed twice: each is memory corruption today, and each can be made a
   refusal for the price of a check.
9. *Where the text forms live.* A model request and reply had to be given a JSON form for
   the prototype, and a tool's content blocks and a session have none it could use.
10. *How a typed service crosses.* What an extension provides to others is an `Arc<T>`
    found by type, so only the registries the mirror has a function for can be reached.

*Direction.* Study it before deciding, with the questions above as the brief. The
recommendation to argue with: in the process, linked at build time, covering tools,
services, gates, prompt parts, secrets and listeners first; the header generated from the
crate and not written by hand; one small extension in C in the tests as the proof that the
mirror types hold; every table carrying a version and its own size, so that it grows by
adding; and the whole of it outside the 1.0 promise until it has been used. That commits
to nothing on loading or on freezing. Whichever way it goes, three things hold: the core
does not change for it, `unsafe` stays in the one crate, and foreign code reaches the
harness only through a `Context`.

## H. The standard set, the command and the agent file

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

`Places::new` looks beside the program and then in the source tree the command was built
from (`crates/corduroy-std/src/places.rs:41`), and only then in the agent file's directory:
a shipped add-on's name resolves to the shipped one before anything a workspace holds
([What is safe by default](design.md#what-is-safe-by-default)). `CORDUROY_PLUGIN_PATH`,
which is the person's, still comes first.

The order gains a step ([How people meet it](design.md#how-people-meet-it)): packages the
person promoted are kept at account level and looked for after the shipped add-ons and
before the project's.

*Direction.* Decide the installed layout (beside the binary, a data directory, embedded),
and with it where the account-level place is on each platform and what the agent file's
strings call it, beside `{workspace}`, `{data}` and `{home}`. Decide what a promoted
package is granted in a workspace it has not been used in. The leaning is that grants
belong to a workspace and are asked for in each, since the same package may be given a
whole tree in one and a single folder in another; what is not settled is whether it brings
any default with it, or starts from nothing each time.

### H7. Are the agent file's older names final?

`[[plugins]]` reaches add-ons only, and the names stay by an earlier decision. 1.0 is the
last moment to change that.

New names arrive with [How people meet it](design.md#how-people-meet-it). Two are chosen:
a value is put into the keychain with `corduroy secrets set`, with `login` kept for what is
signed in to; and the command that tries an agent for real is `corduroy doctor`. The others
are not: the keychain's kind (a `[secrets.*]` entry picks its kind with `kind`), `secret` on
an MCP entry, the manifest's file name and keys, the key by which an agent file lists a
package, and the command that promotes one.

Three of those are built under names that are not yet confirmed: `kind = "keychain"`,
`secret` on an MCP entry, and `keep` on a `chatgpt` secret (`keychain`, or `file` where
that is said).

*Direction.* Confirm the old ones, and choose the new ones in one sitting, saying *add-on*
where an add-on is meant, and by the rule the two chosen ones follow: a name is as far in
meaning from every other command's as it can be, so that no two are taken for each other.

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

The keychain is in the standard set on all three
([How people meet it](design.md#how-people-meet-it)): the macOS Keychain, the Windows
Credential Manager, and the Secret Service on Linux, which a machine with nobody logged in
does not run.

How the keychain kinds are tested is built one way: everything above the system's own store
runs against a keychain in memory, on every platform; one test keeps, reads and removes a
throwaway item in the real one, and runs only where it is asked for
(`CORDUROY_REQUIRE_KEYCHAIN`): in CI on macOS, on Windows, and on Linux inside a D-Bus
session with a keyring made for it. The
Windows store's pieces (a value longer than one credential holds) are tested everywhere
against a small store, and against the real one only there.

*Direction.* Decide whether the full suite on all three is a condition of 1.0, and whether
that one test against the real keychain is enough for the keychain kinds. Find out
what Windows offers for confining a process, and then either promise a confined shell
there or say that 1.0's shell on Windows is the plain one, chosen by the person.

### H15. Which outside conventions does 1.0 commit to?

MCP, the hint files (`AGENTS.md`, `.goosehints`, `CLAUDE.md`), skills as folders, and APIs
in OpenAI's and Anthropic's shapes. Each moves on its own schedule.

The API shapes are in the promise ([How people meet it](design.md#how-people-meet-it)):
five provider kinds, two of them for whatever else speaks OpenAI's or Anthropic's shape.

*Direction.* Name the others that are part of the promise, and the version of each, the
two API shapes included: which of OpenAI's (chat completions, responses) a kind speaks.

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

The first is decided ([How people meet it](design.md#how-people-meet-it)): what the agent
builds is a package, the person's yes is kept for what it may do and not for its code, and
the host loads an approved package again at start.

The scheduler waits: it is an extension that raises signals and starts sessions, the way
for it to do so is decided, and it is not in 1.0
([What an extension can reach](design.md#what-an-extension-can-reach)). An automation the
agent wrote can start a run when an event fires, holding at most its own grants; what it
cannot do until there is a scheduler is wake at a time.

*Direction.* The first two are conditions of the freeze: something the agent built must
still be there tomorrow, and the person must be the one who allowed it. Both are decided
and neither is built; the second waits on the form of the answer (C15).
