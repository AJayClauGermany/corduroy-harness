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

## A. What the freeze is

### A1. Which contracts are frozen, and which are only stable?

[Design](design.md#three-tiers-core-extensions-add-ons) puts four things in the core tier:
`corduroy-core`, the WIT world, the WebAssembly host that enforces it, and the loop
(`corduroy-harness`). They are not equally ready, and they do not all need the same promise.

*Direction.* Two levels. **Frozen** (nothing changes but bug fixes): `corduroy-core`, the
exports and shared types of the WIT world, the log's format. **Stable** (changes only by
adding): the WIT imports, the loop's traits and registries, the host's Rust API,
`corduroy-std`, the agent file. Say for each crate and file which level it is.

### A2. Does "frozen" allow additions?

[What is stable](design.md#what-is-stable) says contracts "change by adding": a new trait
method with a default, a new field on a `#[non_exhaustive]` type. The bar set for 1.0 is
stricter: nothing new is added to the core. Under the strict reading the growth markers are
pointless; under the loose one the core is not really frozen.

*Direction.* Frozen means no new concepts, and additions only at growth points marked before
1.0. That makes sections B to E a list of which growth points to leave open.

### A3. Is the set of capabilities closed at 1.0?

Design says "the mechanism is settled; the set grows". Each capability is an import
interface of the world, which A1 would call stable rather than frozen.

*Direction.* New interfaces may be added after 1.0; an existing interface never changes
(see [E7](#e7-may-an-existing-interface-gain-a-function)).

### A4. What enforces the freeze?

Nothing does today: CI has no check of the public API, the WIT or the log format against a
recorded baseline.

*Direction.* `cargo-semver-checks` (or a public-API snapshot) on `corduroy-core` and
`corduroy-harness`; a snapshot of `wit/corduroy.wit`'s exports and shared types; a log file
of every schema version kept as a fixture and read back in a test.

### A5. What carries the version 1.0, and are the versions tied?

The crates are `0.1.0`, the WIT package is `corduroy:plugin@0.1.0`, the log's schema is `2`.
Whether these move together decides what a guest SDK, a store and an extension each pin.

*Direction.* One version per contract, moving independently: the crates (semver), the WIT
package, the log schema.

### A6. Is the minimum Rust version part of the contract?

The workspace asks for Rust 1.97 and edition 2024. An extension author has to know whether a
core bug-fix release may raise that.

*Direction.* State a policy before 1.0 (for example: a raise is a minor release, never a
patch).

## B. Core: types and traits

### B1. Which public types are closed on purpose?

Seventeen public types cannot gain a field or a case. Each needs a deliberate answer: closed
for good, or `#[non_exhaustive]` with a constructor.

| Type | Where | Direction |
| --- | --- | --- |
| `Event` | `core/event.rs:146` | Open. Design says events gain fields; a store builds one when reading a log back, so it also needs a constructor. |
| `Source` | `core/event.rs:101` | Decide with [B21](#b21-are-the-reserved-names-and-event-sources-a-closed-set). |
| `Role` | `core/model.rs:26` | Decide: are `user` and `assistant` the only roles, with tool results said by the user, for good? |
| `Turn`, `Completion`, `ModelError` | `core/model.rs:35,44,53` | See [B3](#b3-what-does-a-plugin-ask-a-model-and-what-comes-back). |
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
as `ToolCall` and `ToolResult` are.

### B3. What does a plugin ask a model, and what comes back?

The core has two shapes for a conversation. The loop and the log speak `Message` (text,
images, thinking, tool calls). `Models`, which the `llm` capability reaches, speaks `Turn`:
text only, with a `Completion` of a system prompt and turns, answered by a bare `String`
(`core/model.rs:35-84`). So a plugin's model call can never carry an image, report usage,
or say why it stopped, and none of the three types can grow.

*Direction.* One shape: a request built from `Message`s, growable, and a growable reply
(text now; usage and a stop reason later). The `llm` interface in the world follows
([E8](#e8-what-does-llm-carry)).

### B4. Is every core trait synchronous for good?

`Tool::call`, `Service::call`, `Gate::check`, `Model::complete`, `HttpClient::send` and
`Kernel::exec` all block the calling thread. Concurrency is therefore threads, and on a
device with one thread a tool that waits holds up everything, a stop included.

*Direction.* Confirm it, and write down how an asynchronous form would be added without
breaking (a defaulted method, or a second trait), so that it is known to be possible.

### B5. Are there kernels without the standard library?

`kernel` is compiled only with `std` (`core/lib.rs:77`), and `Kernels` is
`Registry<std::sync::Mutex<dyn Kernel>>` (`core/kernel.rs:35`). Design promises a device "a
kernel that is an embedded interpreter"
([Without the standard library](design.md#without-the-standard-library)), which the core
cannot express.

*Direction.* Either split the trait (`exec` and `reset` everywhere; `spawn` and `Process`
with `std`) and take the standard mutex out of the registry's type, or withdraw the promise.

### B6. What does a cell return, and what can a spawned process be given?

A `Cell` is text, a status and a truncation flag (`core/kernel.rs:43`): no image or other
rich output, which a notebook-style kernel produces; no separate error stream; nothing
until the cell ends. A `Spawn` has a program, arguments and environment
(`core/kernel.rs:134`): no working directory. A `Process` has no error stream and no exit
status.

*Direction.* Make `Cell` and `Spawn` growable now. Decide separately whether rich output
belongs in 1.0; the trait can gain defaulted methods later.

### B7. Is a kernel shared by every session?

A kernel is registered under one name, runs one cell at a time, and is shared by a session
and its children: a sub-agent changes its parent's working directory and waits behind its
parent's cells.

*Direction.* Decide whether "a kernel per session" is a core idea or a naming convention
left to extensions (`<kernel>/<session>`), and say so.

### B8. Is an HTTP exchange always one whole body?

`HttpClient::send` returns the whole response or a `String` error
(`core/http.rs:63-71`). There is no stream (server-sent events, a large download) and no
way to tell a timeout from a refused connection. `HttpRequest` and `HttpResponse` cannot
grow.

*Direction.* Make both structs growable and give the error a type. Leave streaming for a
defaulted method later, and confirm that is acceptable for an MCP client written as an
add-on.

### B9. Does a secret say where it may be sent?

`Secret` is `reveal()` and nothing else (`core/secret.rs:20`). A plugin granted a secret
and a broad `http` scope can send that secret to any host its scope reaches.

*Direction.* A defaulted `hosts()` on `Secret`, checked when the host writes the value into
a request. Decide whether the binding is required or optional.

### B10. In what order are prompt parts rendered?

By name (`core/prompt.rs:31-43`), all into the system prompt, afresh on every request. A
part cannot say it belongs first or last, or that it changes on every request, so a
volatile part early in the alphabet defeats a provider's prompt cache for everything after
it.

*Direction.* Decide whether name order is the contract. If not, add a defaulted placement
method before 1.0 so the order has a growth point.

### B11. Are tools one flat, global set?

`Tools` is one registry for the whole agent, with no record of which plugin registered an
entry. Per-session tool sets ([G12](#g12-what-can-differ-between-sessions)) would be built
over it, not in it.

*Direction.* Confirm, and confirm that ownership can be added later as a new method.

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

*Direction.* An emitter bound to the plugin's name that takes the session from the call
scope, so there is one convention before extensions settle on their own.

### B17. Does the call scope follow work to another thread?

It is thread-local (`core/call.rs:56`). An extension that hands a tool call to another
thread loses the session, the waits and the stop flag, and there is no way to carry them.

*Direction.* A handle that captures the current scope and enters it elsewhere.

### B18. Is one call scope for the whole program acceptable without `std`?

Without `std` there is one scope, "wrong for anything that serves two sessions at once"
(`core/call.rs:25-27`).

*Direction.* Confirm it as the contract for devices, or decide what replaces it.

### B19. Is polling the only way to learn of a stop?

`call::stopping()` is asked "now and then". Nothing wakes work that is blocked.

*Direction.* Confirm; a notification can be added.

### B20. How are signals named?

A signal is a bare string (`call::wait_for`). The harness uses names of its own for a
child's end; nothing keeps two extensions' signals apart.

*Direction.* A naming rule (`<plugin>/<what>`), stated before extensions raise signals
([G4](#g4-how-does-an-extension-raise-a-signal-or-resume-a-run)).

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

## C. Core: the event stream and the log

### C1. Is there anything a listener sees that is not logged?

Today no: every event is committed to the `Log` before anyone sees it. Text from a model as
it arrives, or progress from a running tool, would have to be logged token by token or go
around the stream altogether. This is the largest undecided question in the core, and what a
person feels first at the terminal.

*Direction.* Decide between (a) the stream stays durable-only and passing output goes through
a separate channel the loop offers, and (b) a draft may be marked passing: delivered, never
stored, never replayed. Either keeps "the log is the record" true. Try both against a real
provider before choosing.

### C2. Does the stream hold the whole log in memory?

`EventStream` keeps every event (`core/event.rs:344`), `replay` loads them all, and `Log`
can only be written to (`core/event.rs:293-304`). A long-lived host with many sessions, or
a device, cannot let old events go.

*Direction.* Decide what `events()`, `session()` and `matching()` promise (everything, or a
window), and leave a defaulted read side on `Log` so a store can serve what memory dropped.

### C3. In what order are commits from two threads delivered?

Each is delivered by the thread that made it, so a listener may hear the later first
([Design, open questions](design.md#open-questions)). One delivery queue would fix the
order and change when a commit returns.

*Direction.* Decide which is the frozen behaviour.

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

*Direction.* Decide whether the core offers a filter point, or whether a `Log` that wraps
another is the answer (listeners would still see the value).

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

### C10. How long are older shapes read?

Schema 1 (no time) and `message` payloads in oi-kit's shape are still read. Both predate
1.0.

*Direction.* Decide whether 1.0 carries readers for shapes no released version wrote, or
starts clean.

### C11. Which topics and payload fields are frozen, and where is the one list?

The table is in `src/session.rs`, the reference is `docs/reference/events.md`, and the core
emits five topics of its own. They do not agree today (the reference lacks
`plugin/suspended` and `plugin/dropped`).

*Direction.* One list, checked by a test, marked with which topics are contract and which
are diagnostics.

### C12. Which spelling do JSON keys use?

Payloads and gate requests mix `read-only` (`src/gates.rs:65`) with `keep_from`,
`mime_type` and `input_tokens`. After 1.0 neither can be renamed.

*Direction.* Pick one before the log freezes.

### C13. What may a session id contain?

A `SessionId` is any string. Children are named `<parent>/<n>`, and `log-read` scopes are
`<session>:<topic>` with `%` and `:` escaped.

*Direction.* A grammar for ids, so `/` and `:` mean one thing.

### C14. Can a session branch from an earlier point?

Sessions are linear with a parent link. Design holds that branching can be added as "a new
session whose parent is event N of another" without changing the log.

*Direction.* Prove that with a spike before freezing `session/start`.

### C15. Can a person answer an ask at `plugin/load`?

Not yet (design, "still to do"). Asks are answered per tool call (`Harness::answer`).

*Direction.* Decide whether an ask at any declared point can wait for a person, since that
shapes `gate/answer`.

### C16. Is there a timer?

Design lists "a timer that raises signals" as still to do for the core tier.

*Direction.* Decide whether it is the core's, the loop's, or an extension's
([G4](#g4-how-does-an-extension-raise-a-signal-or-resume-a-run)).

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

*Direction.* Decide whether the loop declares those two before 1.0, since gates written by
third parties will key on the names.

### D4. Is name order, first strictest, the final rule?

Gates are asked in name order and the first to give the strictest kind is recorded.

*Direction.* Confirm.

### D5. Can a grant name one method of a service?

The `service` capability is scoped by service name, so a grant gives every method.

*Direction.* Decide whether the resource is `service` or `service:method` before grants are
written against it.

### D6. Does a plugin ship a statement of what it wants?

Design's open question on grant files: the agent file grants; a plugin cannot say what it
needs beyond its imports.

*Direction.* Decide whether this belongs in 1.0's agent file contract or after.

## E. The plugin world

### E1. What can a host call fail with?

`host-error` is `denied` or `failed` (`wit/corduroy.wit:6`), used by fourteen interfaces. A
missing file, bad input and a timeout are all `failed`, and a variant cannot gain a case
without a new package version.

*Direction.* Add the cases now: at least `not-found`, `invalid`, `timed-out`.

### E2. Can what a guest returns grow?

`tool-spec` and `plugin-info` are fixed records, though the core's `ToolSpec` was made
growable. `content` is text or an image.

*Direction.* An open tail on both records (a list of named values), and a general typed
block (`mime-type`, `data`) beside `image`.

### E3. What happens to a guest built for another version of the world?

The host drops the version when reading a component's imports
(`crates/corduroy-wasm/src/plugin.rs:397`), so such a guest loads and then fails at
instantiation with the engine's own error.

*Direction.* Check at load, with a message that names both versions; decide which versions a
host accepts.

### E4. How is a new kind of contribution added once exports are frozen?

All six exports are required of every guest, so a seventh orphans every built add-on.

*Direction.* Choose one rule and write it down: the host resolves exports one by one and
tolerates absence, or every new kind goes through `provides` and `handle-call` as
`compaction` already does.

### E5. Can a header mix text and a secret?

A `header-value` is all text or all secret (`wit/corduroy.wit:333`), so
`Authorization: Bearer <secret>` works only if the stored secret includes `Bearer `.

*Direction.* A value as a list of text and secret parts.

### E6. Can an add-on's tool call wait for a signal?

Natively, yes (`call::wait_for`). The world has no such call; an add-on waits only by
starting a child session.

*Direction.* Decide whether waiting is offered to add-ons (on `session`, or a new
interface) before extensions start raising signals.

### E7. May an existing interface gain a function?

"Adding an import interface breaks nobody" is the stated rule. Adding a function to an
interface that exists is not covered, and several entries below depend on it.

*Direction.* Decide: either interfaces are frozen and growth is a new interface, or an
interface may gain functions under a new minor version.

### E8. What does `llm` carry?

Text in, a string out (`wit/corduroy.wit:300-318`), mirroring
[B3](#b3-what-does-a-plugin-ask-a-model-and-what-comes-back).

*Direction.* Decide with B3.

### E9. Can an add-on read bytes?

`fs-read` returns text. Reading an image (design's `read_image`, "left for later") needs
bytes.

*Direction.* Decide whether this is in 1.0; under E7 it is either a new function or a new
interface.

### E10. Who bounds a kernel cell's time?

The guest passes any timeout it likes to `kernel.exec`; the host adds no bound, unlike `llm`
and `http`.

*Direction.* A host limit, and decide whether the parameter stays.

### E11. Should every host call be able to fail?

`tools.list`, `plugins.loaded` and `session.stopping` return plain values; they can never
report a refusal.

*Direction.* Decide whether that is deliberate before the signatures freeze.

### E12. Can an add-on ever contribute a loop step, a model or a kernel?

Today no: these are native. Design's open question ("operations from plugins") needs an
export, and exports freeze.

*Direction.* Decide now whether the answer is "never", or "through services" (E4), so no
export has to be added later.

### E13. When is a value JSON text and when a typed record?

Gate requests, service payloads and configuration are JSON text; events and tool specs are
records.

*Direction.* State the rule (JSON where the shape is the plugin's or grows; records where
the host must read it).

### E14. Can a running tool report progress?

It can learn it is stopping; it cannot say how far it has got.

*Direction.* Decide with [C1](#c1-is-there-anything-a-listener-sees-that-is-not-logged):
progress is the same kind of passing output.

### E15. What trail does a tool call made by an add-on leave?

A call through `tools.call` is gated with no session and leaves no `tool/start`
(`crates/corduroy-wasm/src/plugin/host.rs:439`), unlike a call the model makes.

*Direction.* The same trail as any call, with the session from the call scope.

### E16. Are loads by a plugin durable?

A plugin loaded by a plugin is gone after a restart unless its loader loads it again.

*Direction.* Decide whether the host owes this (a plugin store), since `plugins.load`'s
meaning freezes with the interface.

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

## G. The loop and the extension tier

### G1. What does an `Operation` see?

`run(&self, session: &Session)` (`src/operation.rs:77`), with no room for more. The built-in
steps reach further, through crate-private parts.

*Direction.* A context argument, as `Loop` has `Run`.

### G2. What of the journal is public?

`gated`, `compactable`, `transcript_before`, `start_call`, `finish_call` and `wait_call`
are crate-private (`src/session.rs:1109-1256`), and `Sessions` is not a service. So an
extension cannot write the compaction strategy design promises, nor run a tool with the
trail the architecture rules demand.

*Direction.* Decide the public read side of a session and the public form of a tool call's
trail.

### G3. How may a loop run tools?

Only through `Run::call_tools`: every call of a message, one after another
(`src/tools.rs:52`).

*Direction.* A single-call primitive, and a decision on whether calls of one message may
run in parallel (which the log's order must then allow for).

### G4. How does an extension raise a signal or resume a run?

Only `Harness::signal` does, and a plugin has no harness. Timers, webhooks and background
jobs therefore cannot be extensions of the shipped command.

*Direction.* A service the harness provides for signals, and a defined owner for taking the
run on once it arrives.

### G5. Which harness types are in the stable set?

[What is stable](design.md#what-is-stable) names `Operation`, `Operations`, `Loop`, `Loops`,
`Provider`, `Providers`. An embedder also depends on `Harness`, `Run`, `Journal`, `Session`,
`Status` and `Outcome`.

*Direction.* List them, and make the ones that will grow growable: `HarnessError`
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

*Direction.* A defaulted streaming method on `Provider`, once
[C1](#c1-is-there-anything-a-listener-sees-that-is-not-logged) says where the pieces go.

### G10. Why did the model stop, and why did it fail?

`ModelReply` has no stop reason, so a reply cut off at its token limit ends the run as
finished. `ProviderError` is text, so a loop cannot tell a full context from a rate limit
or a bad key.

*Direction.* A stop reason on the reply, and a kind on the error.

### G11. What can be set per request?

`ModelRequest` carries messages, tools and a system prompt. The token limit, temperature,
thinking, tool choice and a required output shape are fixed per provider entry.

*Direction.* Decide which belong on the request (it is growable) and which stay
configuration.

### G12. What can differ between sessions?

Nothing: model, provider, system prompt, loop and tools are the harness's, and a child
session gets the same. A sub-agent with fewer tools or a cheaper model cannot be expressed.

*Direction.* Decide what `session/start` may carry, since that is in the log.

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

### G16. How do child sessions run?

One at a time, at most three deep (`MAX_DEPTH`, a constant in `src/children.rs`).

*Direction.* Decide whether depth is configuration, and whether children of one message may
run together.

### G17. Where is usage added up?

Per request, as `usage` events. `Outcome` has no total, and a plugin's `llm` calls record
none (see [B3](#b3-what-does-a-plugin-ask-a-model-and-what-comes-back)).

*Direction.* Decide whether totals are the harness's to keep or a reader's to compute.

## H. The standard set, the command and the agent file

### H1. Which provider kinds and keys are contract?

`[providers.*]` is the one section not built from a kind of the `Standard`
(`crates/corduroy-cli/src/agent.rs:645`). Its keys follow oi-kit's settings, and "other
providers oi-kit knows can be named directly as the `kind`": the agent file, a stable
contract, inherits whatever an alpha dependency accepts.

*Direction.* Provider kinds in `Standard` like the rest, and a named list of kinds and keys
that are ours.

### H2. Does 1.0 ship on oi-kit?

`corduroy-providers` is pinned to an exact alpha. It is confined to one crate behind
`Provider`.

*Direction.* Decide whether that is acceptable for 1.0 or whether clients of our own for
two or three APIs come first.

### H3. What of the harness can the agent file set?

Provider, model, system prompt and turn limit. Not the loop (so a `Loop` an extension
registers can never be chosen), the restart limit, extra steps, or the WebAssembly limits.

*Direction.* Decide which become keys and which are a `Standard` part.

### H4. Do all credentials go through `Secrets`?

Provider keys and an MCP server's bearer token are read from the environment directly, so
replacing `env-secret` with a keychain covers neither.

*Direction.* One path for every credential.

### H5. Can the `Standard`'s own types grow?

`Asked` and `Places` (`crates/corduroy-std/src/`), `Transport` (`corduroy-mcp`),
`ProviderSpec` and `Thinking` (`corduroy-providers`) are built by literal.

*Direction.* Growable, with constructors.

### H6. Where does an installed command find the shipped add-ons?

`Places::new` ends the plugin path with the source tree the command was built from
(`crates/corduroy-std/src/places.rs:39`).

*Direction.* Decide the installed layout (beside the binary, a data directory, embedded).

### H7. Are the agent file's older names final?

`[[plugins]]` reaches add-ons only, and the names stay by an earlier decision. 1.0 is the
last moment to change that.

*Direction.* Confirm.

### H8. How much of MCP is in 1.0?

Tools, listed once at start. No resources, prompts, sampling, list changes or reconnection.

*Direction.* Decide the scope; all of it is addable in the extension.

### H9. Is a sandboxed kernel part of the standard set?

Protected paths bind `fs-read` and `fs-write`; "protecting paths from a kernel is the
kernel's job". The shipped shell kernel is not sandboxed, so an agent with `bash` can read
its own log and agent file.

*Direction.* Decide whether 1.0 needs one, or says plainly that it has none.

### H10. Which platforms does 1.0 promise?

CI runs the whole suite on Linux; on Windows and macOS it builds the command and tests the
core and the PowerShell kernel.

*Direction.* Decide whether the full suite on all three is a condition of 1.0.

## I. Add-ons and the guest SDKs

Add-ons may keep changing after 1.0. These are the few things about them that others will
depend on.

### I1. Is `corduroy-guest`'s API stable?

Every Rust add-on builds on it. It is neither in the stable list nor declared free to move.

*Direction.* Decide which, and version it against the WIT package.

### I2. How do the SDKs find the world, and how are they versioned?

Both read `wit/corduroy.wit` from a path relative to this repository, which a published
package would not have, and their versions are independent of the WIT package's.

*Direction.* Embed the world in each SDK and tie the versions.

### I3. Are the shipped add-ons' tool names and configuration stable?

They are in the reference and in the default agent file people will copy.

*Direction.* Say explicitly that they are not, or name the ones that are.
