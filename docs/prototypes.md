# Prototypes: open questions tried in code

Some of the [open questions](open-questions.md) cannot be answered well by thinking about
them: the answer is a form (a trait's signature, a key's name, a field in the log), and what
is wrong with a form shows when something is built against it. A prototype is one form of
an open answer, built so that it can be used and argued with. This page lists each one:
the question it is for, what was built and where, what building it already answered, what
it found that nobody had asked, and what is still a person's to decide.

**A prototype decides nothing.** Its question stays in [Open questions](open-questions.md),
with a line that points here. Three kinds of page are kept apart:

| Page | Holds | An entry leaves when |
| --- | --- | --- |
| [Design](design.md) | what is decided, built or not | never |
| [Open questions](open-questions.md) | what is still to decide, with a direction | it is decided |
| This page | one built form of an open answer, and what it found | its question is decided |
| [Trials](trials.md) | a question the code offers both answers to, to be tried by use | a person has said which |

The rules a prototype is held to:

- *It is marked where it lives.* The code's own documentation says "a prototype" and names
  the question, so that nobody reading the code takes it for a contract.
- *It changes a stable contract only by adding,* and only where the decision it serves says
  the addition belongs there. Nothing here changes the plugin world (`wit/corduroy.wit`).
- *It leaves what exists working.* An agent file, a log or a keychain item from before it
  reads and runs as it did.
- *It is promised to nobody.* A name, a key or a field on this page may change or go.
- *When its question is decided,* the answer is written where it belongs, the code is made
  to match it or taken out, and the entry is deleted from this page.

| | Question | What is built | Where |
| --- | --- | --- | --- |
| [X1](#x1-a-secret-names-the-hosts-it-may-go-to) | [B9](open-questions.md#b9-how-does-a-secret-say-where-it-may-be-sent), with [T4](trials.md#t4-is-a-secrets-name-the-right-name-for-its-item) | A secret names its hosts, and a provider, an MCP server and an add-on's request are refused it for any other | `corduroy-core` (`secret`), `corduroy-wasm`, `corduroy-std`, `corduroy-keychain`, `corduroy-providers`, `corduroy-mcp`, the command |
| [X2](#x2-shaping-a-request) | [G19](open-questions.md#g19-who-decides-what-the-model-sees) | Steps that change what the model is sent, registered by name, with what they did recorded | `src/shape.rs`, `src/loops.rs` |
| [X3](#x3-one-list-of-topics-checked) | [C11](open-questions.md#c11-which-topics-and-payload-fields-are-frozen-and-where-is-the-one-list) | A test that holds the events reference to the topics the code raises | `crates/corduroy-cli/tests/topics.rs` |
| [X4](#x4-the-trusted-base-measured) | [A4](open-questions.md#a4-what-enforces-the-freeze) | A script that measures the trusted base and compares it with a budget | `scripts/trusted-base.sh` |

[Built from decisions](#built-from-decisions-and-what-that-left-open) lists three things
that were decided and are now built, each of which made a choice an open question still has
to confirm. [What could be prototyped next](#what-could-be-prototyped-next) says how the
next questions could be tried.

---

## X1. A secret names the hosts it may go to

*For:* [B9](open-questions.md#b9-how-does-a-secret-say-where-it-may-be-sent). Decided
([What is safe by default](design.md#what-is-safe-by-default)): a secret names the hosts it
may go to; one that names none goes nowhere; a keychain secret's hosts are said by the
person when they put the value in, and kept beside it; one from the environment says them
in the agent file. Open: the form.

**The form tried.**

- *In the core,* `Secret::hosts()`, a defaulted method that returns a `Scope` over host
  names: the same type, and so the same patterns and the same `deny`, as an `http` grant.
  `Secrets::may_go(name, host)` is the one check, and `secret::to_hosts` holds any secret
  to a scope.
- *Checked wherever a value is written into a request:* an add-on's `http` request (the
  WebAssembly host, per header that carries a credential), a provider's key (against the
  host of its `base_url`, or of its kind's own address), and an MCP server's bearer token
  (against the host of its `url`).
- *Said in the agent file* for a secret read from the environment:
  `hosts = ["api.github.com"]` in its `[secrets.<name>]` entry.
- *Said beside the value* for one in the keychain:
  `corduroy secrets set <name> --hosts api.anthropic.com`, kept as a second item,
  `<name>.hosts`, one host to a line. `corduroy secrets` says where such a value goes.

**Try it.**

```bash
corduroy secrets set anthropic --hosts api.anthropic.com
```

```bash
corduroy secrets
```

Then point the provider somewhere else (`base_url = "https://example.org"` in its entry)
and run: the request is refused before the key is sent, saying which secret and which host.

**What building it answered.**

- A defaulted method on `Secret` is enough: no registry, no new capability and no change to
  the plugin world. An add-on sees a refusal as `denied`, as it sees any other.
- A `Scope` fits. Nothing about hosts needed a type of its own, and a secret's hosts and an
  add-on's `http` grant are then read by one rule.
- The gap [T4](trials.md#t4-is-a-secrets-name-the-right-name-for-its-item) describes
  closes: with hosts kept beside the value, an agent file somebody else wrote can name
  your key and cannot send it elsewhere (`crates/corduroy-cli/tests/keychain.rs`).

**What it found.**

1. *`hosts()` cannot fail, and a keychain can.* A keychain that cannot be read has to
   answer something. The prototype answers "none" (so nothing is sent) and has every check
   reveal the value first, so that the keychain's own reason is what the person reads and
   not "does not name this host". Either `hosts()` returns a `Result`, or "value first,
   then hosts" is a rule every place that sends a secret has to keep.
2. *A second item is a second read.* A keychain secret was read once for a run. It is now
   read twice, the hosts and the value, and on macOS each read of an item may ask the
   person ([T0](trials.md#t0-does-macos-ask-after-a-rebuild) has not seen it yet). One item
   holding both would be one read, and would no longer be the bare key that another
   program reading the same item expects.
3. *"Says nothing" and "names none" had to be told apart.* The decision is that a secret
   naming no host goes nowhere. Turning that on at once would stop every agent file and
   every keychain item there is, so the prototype keeps three states: hosts named, none
   named (`hosts = []`, `--hosts ""`), and nothing said, which is sent as before. The
   decided end is one line (`Secrets::may_go` reading "nothing said" as "none"). Before it:
   `corduroy init` has to write hosts, a `chatgpt` secret has to know its own (today it
   says nothing), and items already in a keychain need a way to be given theirs.
4. *An item cannot be given hosts without typing its value again.* `secrets set --hosts`
   asks for the value, as `secrets set` does. Hosts for a value already there need a
   command or a flag of their own.
5. *A host is a name, and a request has more.* The check sees the host: not the scheme, not
   the port. A secret named for `api.example.com` also goes to `http://api.example.com:81`.
6. *`*` crosses dots.* The pattern is the grant's, so `*.example.org` does not reach
   `example.org`, and `*example.org` reaches `evilexample.org`. That is B12's question,
   met again with a credential behind it.
7. *The three refusals are recorded three ways.* An add-on's is an event of its own
   (`secret/refused`); a provider's ends the run as failed, with the reason; an MCP
   server's stops the server from starting. None is offered to the person, which the
   decision asks for and which waits on an ask that can wait for an answer (C15).
8. *An environment secret's hosts are in the file that uses it.* They hold an add-on with a
   wide `http` grant to the hosts named. They do nothing against the file itself, which is
   as decided: such a file is one the person wrote or accepted.

**Still to decide.** Exact names or patterns, and with which syntax (B12). Whether the port
and the scheme are part of what a secret names. One keychain item or two. The names: the
key `hosts`, the flag `--hosts`, the item `<name>.hosts`, the topic `secret/refused`. What
becomes of a value put in before it had hosts, and when "nothing said" stops being sent.
Whether a provider's refusal should be an event as an add-on's is.

**Not built.** The shell's side (a kernel's filter and masked credentials wait on the
confined kernel), hosts for a `chatgpt` secret, `init` writing hosts, and offering a
refused host to the person.

## X2. Shaping a request

*For:* [G19](open-questions.md#g19-who-decides-what-the-model-sees). Decided
([Keeping within what the model holds](design.md#keeping-within-what-the-model-holds)):
shaping a request is a step, registered as an `Operation` is; it changes what is sent and
not the record; it is not a ninth kind of thing a plugin contributes. Open: the registry
and the trait, the order, what a step may change, what is recorded, and whether an add-on
can be one.

**The form tried.**

- *A trait and a registry in the harness:* `Shape::shape(&self, session, request) ->
  request`, called and waited for, never failing; `Shapers`, provided as the service
  `shapers`, so a plugin registers one through its `Context` and unloading takes it away.
- *Name order,* each shaper handed what the one before gave back.
- *A shaper may change* the messages, the system prompt and the tools offered. It may not
  change the model: the harness puts the session's back.
- *The standard loop runs them* before every request (`Run::shape`, then
  `Run::ask_shaped`). `Run::ask_model` is as it was, so a loop of one's own shapes a
  request only if it says so.
- *Recorded on `model/request`,* and only for a request something changed: `shaped`, the
  names of the shapers that changed it in the order they ran, and `messages_hash`, a hash
  of the messages sent. The system prompt and the tools were already named by hash.

**Try it.** `tests/shaping.rs` has three shapers of a few lines each: one that sends the
last message only, one that adds to the system prompt, and one a plugin contributes.

**What building it answered.**

- The decision holds as written. The session's projection, the log and resuming needed no
  change: a request a shaper cut is whole again as soon as the shaper is gone.
- It is not a new kind in the core: the registry is the harness's, as `Operations` and
  `Loops` are.
- The existing hashes cover two of the three parts. `system_hash` and `tools_hash` are
  taken from what is sent, so they were already right for a shaped request; only the
  messages needed something new.

**What it found.**

1. *The record says that a request was shaped, and not how.* `messages_hash` can be
   compared with nothing in the log, since the projection's own hash is not recorded. A
   reviewer learns that the model saw something other than the conversation and has to
   rebuild the projection to learn what. Recording which messages were left out (their
   event ids) would say it; the session's snapshot with ids, which is decided and not
   built, is what a shaper would need to give them.
2. *Knowing which shaper changed the request costs a copy.* The harness compares the
   request before and after each shaper, which copies the whole conversation once for each
   shaper on every request. A shaper that says whether it changed anything would cost
   nothing, and could lie.
3. *A loop that does not ask for shaping gets none.* That follows the decision ("a loop an
   extension wrote may"). It also means a person who loads a trimming plugin and a custom
   loop gets no trimming and no word about it.
4. *A shaper cannot mark where a provider may cache,* which the decision names as one use.
   The core's `ModelRequest` has no place for such a mark: that is
   [G11](open-questions.md#g11-what-can-be-set-per-request).
5. *It was not handed what it is to be handed.* A shaper gets the whole `Session`. An
   operation is to get a context in its place
   ([What an extension can reach](design.md#what-an-extension-can-reach)), and a shaper
   should get the same one when that is built.
6. *Called and waited for is enough for the uses named.* Dropping, trimming and adding a
   reminder need no model and no waiting. Something that asks a model to shorten the
   context is compaction, which is in the record on purpose.

**Still to decide.** Name order, or an order a shaper states (with B10). Whether the system
prompt and the tools are a shaper's to change. Whether every request is shaped, whichever
loop asks. What the record holds of a shaped request. The names `shapers`, `Shape`,
`shaped` and `messages_hash`.

**Not built.** An add-on as a shaper, through `handle-call` under a name the harness
defines ([E18](open-questions.md#e18-which-service-names-are-the-harnesss)): the request
would cross as JSON on every model call, which is the cost to measure first. A shipped
shaper: nothing in the default agent uses the registry.

## X3. One list of topics, checked

*For:* [C11](open-questions.md#c11-which-topics-and-payload-fields-are-frozen-and-where-is-the-one-list).
Its direction asks for one list of topics, checked by a test.

**The form tried.** The [events reference](reference/events.md) is the list. A test reads
the host's sources for string literals of a topic's form (`<prefix>/<what>`, under the
prefixes the core, the loop and the standard extensions use) and the page for the topics it
names, and fails if either has one the other lacks. It runs with `cargo t`.

**What building it answered.** The check is cheap (a hundred lines, no dependency) and it
found something the first time it ran: `plugin/suspended` and `plugin/dropped` were raised
and not in the reference, which C11 had noted by hand. Both are written down now, as are
the two topics this round added (`plugin/missed`, `secret/refused`).

**What it found.**

1. *Reading sources as text is as far as this form goes.* It finds a topic written as a
   literal. A topic put together at run time, or one of the four of one word (`message`,
   `signal`, `usage`, `compaction`), is not found.
2. *The list that would make it exact is a table in the code:* each topic a constant, with
   its source and whether it is contract, which the code refers to and the page is checked
   against. That table is also where C8's reserved prefixes would be read from.
3. *It says nothing of payloads,* which is the larger half of C11.

**Still to decide.** Which topics are contract and which are diagnostics; the test can hold
a column once somebody fills it in. Whether the list lives in the code or on the page.

## X4. The trusted base, measured

*For:* [A4](open-questions.md#a4-what-enforces-the-freeze). Decided
([What is safe by default](design.md#what-is-safe-by-default)): the trusted base is kept to
a budget of lines and dependencies, the WebAssembly engine counted with them. Open: the
figures, and the check that holds them.

**The form tried.** `scripts/trusted-base.sh` counts lines of code (not blank, not a
comment, not a file's unit tests) in the core, the WebAssembly host, the loop and the
plugin world, and the crates each needs. With `--budget` it compares them with
`scripts/trusted-base.budget` and fails if one is over. It is not part of
`scripts/check.sh`.

```bash
scripts/trusted-base.sh
```

**What it measured** (28 November 2026):

| | Lines | | Crates from outside |
| --- | --- | --- | --- |
| The core | 2967 | The core | 0 |
| The WebAssembly host | 2171 | The loop without add-ons | 19 |
| The loop | 4800 | The loop with add-ons | 110 |
| The plugin world | 230 | | |
| Together | 10 168 | | |

**What it found.**

1. *The engine is the budget.* Of 110 crates, 91 come with the WebAssembly engine. A
   budget of dependencies is in practice a statement about wasmtime's tree, which moves
   with each of its releases and is not ours to hold down.
2. *The loop is the largest part,* at close to half the lines, and `src/session.rs` alone
   is a seventh of the whole. The prototypes on this page added to it.
3. *What counts has to be said before a figure means anything.* The measure leaves out the
   store (`corduroy-jsonl`), which keeps the record; the standard extensions, which hold
   the host's authority; and the provider library. Each is arguably trusted.

**Still to decide.** The figures: the budget file holds the day's measurements, which is
not a decision. Whether dependencies are counted with the engine, without it, or both.
Which crates are the trusted base. Whether going over fails the check or is only reported.

---

## Built from decisions, and what that left open

Three things were decided and are now built. Building each meant choosing something an
open question has not settled; the choices are listed so that they are confirmed or changed
on purpose and not inherited.

**A grant can name one method of a service** (decided in
[What an extension can reach](design.md#what-an-extension-can-reach)). Built in the core
(`Scope::permits_part`, `Grants::check_part`) and the WebAssembly host. A pattern that
matches a service's name speaks for every method of it, for `deny` as for `allow`.

- [B12](open-questions.md#b12-what-does-a-pattern-mean-for-good) asks what `:` means in a
  service's or a method's name. Nothing refuses one yet, so a service named `a:b` and the
  method `b` of a service `a` are one resource. `sessions`, `sessions:*` and `*` are the
  same grant, and a test says so.

**An add-on does not miss an event because it was busy** (decided in
[Events, gates and hooks](design.md#events-gates-and-hooks)). Built in the WebAssembly host
(`crates/corduroy-wasm/src/plugin/instance.rs`): an event that arrives while the add-on is
in a call is kept and handed over when the call returns, in order.
[F6](open-questions.md#f6-how-long-does-a-sessions-instance-of-an-add-on-live) asks four
things of that queue, and the code answers each one way:

- *How many it holds:* 1024 (`Limits::missed_events`). One more and the oldest goes, with a
  `plugin/missed` event naming it.
- *What becomes of them when the instance traps:* they are kept, and the instance started
  in its place hears them.
- *When the add-on is stopped:* they are dropped.
- *Whose queue it is:* the add-on's, since there is one instance today. With an instance
  for each session it has to be said again.

**`write` and `edit` refuse a file that changed since it was read** (decided in
[What an extension can reach](design.md#what-an-extension-can-reach)). Built in `pi-tools`,
for what one instance has seen.

- The decision says "since the session last read it". An add-on learns its session only
  through the `session` capability, which `pi-tools` does not import, and importing it
  would stop every agent file there is until it granted it. So what was seen is kept in
  the instance's memory, and one instance serves every session: a change made by a shell
  command, another add-on or the person's editor is caught, and a change made by another
  session through these same tools is not. It becomes each session's own when an add-on
  has an instance for each session (F6), with no change to `pi-tools`.
- What was seen is forgotten when the instance is restarted, and then nothing is refused.
- A file is known by the path it was given under, so one file reached by two spellings is
  two files. `./a` and `a` are told to be one; a symbolic link is not.

## What could be prototyped next

Open questions that building would inform, with the smallest thing that would. None is
started.

| Question | What to build | What it would answer |
| --- | --- | --- |
| [C15](open-questions.md#c15-can-a-person-answer-an-ask-at-pluginload), [G6](open-questions.md#g6-what-can-a-person-answer-to-an-ask) | An ask at `plugin/load` that is recorded and waits, listed by `Outside` beside a tool call's, and answered by one `gate/answer` | The fields by which an answer names what it is for, with old logs still read; what `plugins.load` hands back while the person has not answered (E16). It is also what the fourth guarantee waits on |
| [D3](open-questions.md#d3-which-decision-points-exist-at-10) | `session/start` declared by the harness and asked where the `sessions` service starts a child | The request's JSON (D2), with only a parent and a task to put in it until a session is started with more |
| [E12](open-questions.md#e12-can-an-add-on-ever-contribute-a-loop-step-a-model-or-a-kernel) | The test-driven pair written as a description, and one native loop that carries a description out | What a description cannot say, which is the list E12 asks for |
| [B17](open-questions.md#b17-does-the-call-scope-follow-work-to-another-thread) | A handle that captures the call scope and enters it on another thread | Whether it is an addition to `call`, and what a tool that fans out has to do |
| [C14](open-questions.md#c14-can-a-session-branch-from-an-earlier-point) | A `session/start` that names an event of another session, and a projection that reads up to it | Whether the log changes, as Design holds it does not |
| [G17](open-questions.md#g17-where-is-usage-added-up) | A total on `turn/end`, beside the report's own sum ([T8](trials.md#t8-who-adds-up-what-a-run-used)) | Whether a reader's sum and a recorded one ever differ, and by what (an add-on's model calls) |
| [E1](open-questions.md#e1-what-can-a-host-call-fail-with) | The three cases added to `host-error` | Nothing building would: it is a decision, and costs a rebuild of every guest in three languages |
