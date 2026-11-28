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
| [X5](#x5-the-decision-point-before-a-session-starts) | [D3](open-questions.md#d3-which-decision-points-exist-at-10), [D2](open-questions.md#d2-are-the-gate-requests-json-shapes-contract) | `session/start` declared by the harness, and the gates asked before a child session starts | `src/children.rs` |
| [X6](#x6-a-calls-scope-carried-to-another-thread) | [B17](open-questions.md#b17-does-the-call-scope-follow-work-to-another-thread) | A handle that takes what a thread serves to another thread | `corduroy-core` (`call`) |
| [X7](#x7-what-a-run-used-on-its-end) | [G17](open-questions.md#g17-where-is-usage-added-up), with [T8](trials.md#t8-who-adds-up-what-a-run-used) | A total of what a run's requests used, on its `turn/end` | `src/session.rs` |
| [X8](#x8-a-session-that-branches-from-another) | [C14](open-questions.md#c14-can-a-session-branch-from-an-earlier-point) | A session that begins where another stood at an event | `src/session.rs` |
| [X9](#x9-an-ask-that-reaches-a-person-anywhere) | [C15](open-questions.md#c15-can-a-person-answer-an-ask-at-pluginload), [E16](open-questions.md#e16-are-loads-by-a-plugin-durable) | An ask at `plugin/load` or `session/start` that is recorded, answered by a person, and found answered when the same is asked for again | `corduroy-core` (`gate`), `src/asks.rs`, `corduroy-wasm` |

[Built from decisions](#built-from-decisions-and-what-that-left-open) lists three things
that were decided and are now built, each of which made a choice an open question still has
to confirm. [What waits on a decision](#what-waits-on-a-decision) says what has to be
chosen before the next question can be tried.

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
| The core | 3040 | The core | 0 |
| The WebAssembly host | 2188 | The loop without add-ons | 19 |
| The loop | 5255 | The loop with add-ons | 110 |
| The plugin world | 230 | | |
| Together | 10 713 | | |

**What it found.**

1. *The engine is the budget.* Of 110 crates, 91 come with the WebAssembly engine. A
   budget of dependencies is in practice a statement about wasmtime's tree, which moves
   with each of its releases and is not ours to hold down.
2. *The loop is the largest part,* at close to half the lines, and `src/session.rs` alone
   is a sixth of the whole. What this page describes added to it: the three decided
   items and the prototypes came to some 840 lines of the trusted base, of 9871 before.
3. *What counts has to be said before a figure means anything.* The measure leaves out the
   store (`corduroy-jsonl`), which keeps the record; the standard extensions, which hold
   the host's authority; and the provider library. Each is arguably trusted.

**Still to decide.** The figures: the budget file holds the day's measurements, which is
not a decision. Whether dependencies are counted with the engine, without it, or both.
Which crates are the trusted base. Whether going over fails the check or is only reported.

## X5. The decision point before a session starts

*For:* [D3](open-questions.md#d3-which-decision-points-exist-at-10) and
[D2](open-questions.md#d2-are-the-gate-requests-json-shapes-contract). Decided
([What an extension can reach](design.md#what-an-extension-can-reach)): the loop declares
`session/start`, and the gates are asked with who is starting the session, its parent and
what it would be started with. Open: the request's JSON.

**The form tried.** The harness declares the point, and the `sessions` service asks the
gates before it starts a child, with `{"session", "parent", "starter", "task"}`: the id the
session would have, the session whose tool call starts it, who asks (`plugin:subagents`)
and what it is for. A denied session is not started, and the call fails with the gate's
reason, which the model reads. The decision is recorded as a `gate/decision` in the session
that asked. A gate that asks is refused, as at `plugin/load`.

**Try it.** `tests/subagents.rs` has a gate of a few lines that lets the session a person
started start sessions and no session started that way: the policy "an orchestrator starts
no orchestrators", which the decision names as a gate's to say.

**What building it answered.**

- It is the loop's point, declared with no change to the core, and a gate written for
  `tool/call` is not asked at it.
- The policy the decision gives as its example can be written from the request as it is:
  the parent's id says how deep the starter is.

**What it found.**

1. *Most of the request does not exist yet.* The decision lists a model, a loop,
   instructions, grants and limits. A child is started with a task and nothing else today,
   so "a reviewer does not run on its starter's model" cannot be written until a session
   is started with one.
2. *The starter is always a plugin.* The person's own session is there before any gate is
   loaded, and a session started from outside is not built, so `starter` has one form.
3. *An ask had nowhere to wait.* It was a denial, with a reason that says so, as at
   `plugin/load`. [X9](#x9-an-ask-that-reaches-a-person-anywhere) gives both points one
   way to reach a person, where the host takes answers.
4. *The decision is recorded without a call.* A `gate/decision` has named a tool call
   until now (`id`, `tool`). This one names a session, and readers that take `id` for
   granted (the report's "waited on" list, for an ask) have to learn the other shape.
5. *The gate is asked without the tool call's scope,* for the parent session only, so that
   a gate cannot make the call it is asked about wait.
6. *A gate that starts sessions cannot be asked about its own.* It is in a call when the
   question comes, so it cannot answer and the start is denied. That is right, and worth
   a check at activation like the one for `tool/call` and `tools`.

**Still to decide.** The request's fields and their names. Whether a session the person
starts with the command is asked about too. What an ask does (with C15).

## X6. A call's scope carried to another thread

*For:* [B17](open-questions.md#b17-does-the-call-scope-follow-work-to-another-thread). Its
direction: a handle that captures the current scope and enters it elsewhere, in the core
before the freeze.

**The form tried.** `call::carry()` takes what the thread serves (the session, the tool
call, what stops the run, what bounds the result), and `Carried::enter(work)` serves it on
whichever thread calls it, for as long as `work` runs. `enter` hands back the signals the
work said the call waits for.

**Try it.** `fan` in `tests/subagents.rs` is a tool that does its work on a thread of its
own.

**What building it answered.**

- It is an addition to `call`: two functions and a type, nothing existing changed, and it
  builds without the standard library.
- A stop reaches the other thread, an outer call's included, because the flag is shared
  and not copied.

**What it found.**

1. *Waits do not cross by themselves.* A wait said on the other thread is collected there.
   The tool has to say it again on the thread it was called on, or the run does not wait:
   the call returns at once and the signal, when it comes, answers nothing. The test shows
   both. A handle that carried the waits back would need the two threads to share them,
   which the scope does not do today.
2. *Nothing makes a tool carry its scope.* A thread started without it serves no session,
   in silence. Once the scope carries a session's grants, that is work done under no
   limits, which is what B17 warns of. The host could refuse a host call made with no
   scope from a thread that is not the host's own; that is a rule to decide, not a
   mechanism to add.
3. *The handle outlives the call.* It can be kept and entered after the call has returned,
   and work done then is attributed to a call that is over.
4. *Not tried:* an async tool whose work moves between an executor's threads, which is the
   case a native tool on tokio would meet first.

**Still to decide.** Whether this is the handle. Whether a wait said elsewhere reaches the
call by itself. What a host call with no scope is served as. The names `carry`, `Carried`
and `enter`.

## X7. What a run used, on its end

*For:* [G17](open-questions.md#g17-where-is-usage-added-up). It asks whether totals are
the harness's to keep or a reader's to compute. [T8](trials.md#t8-who-adds-up-what-a-run-used)
offered the reader's sum; this is the other way, so both can be looked at.

**The form tried.** A `turn/end` gains `used`: `{"requests"}` and each count a `usage`
event gives (`input_tokens`, `output_tokens` and the rest), added up over the model
requests of the run that answered. It counts the session's own requests.

**What building it answered.**

- The total is the same whether a run went through in one process or was picked up from
  the log by another: it is added up from the `usage` events either way.
- A log line gained a field and nothing that reads the log had to change.

**What it found.**

1. *The two sums differ, on purpose or not.* The report's sum has what the sessions a run
   started used; a `turn/end` has its own session's. Neither has what an add-on's own
   model calls used (`summarize`, `review`, `compact`). So "what did this cost" has three
   answers today, and a total the harness keeps has to say which it is. That is also what
   G20 asks of a child's budget.
2. *Five tests compared a `turn/end` exactly* and had to be told of the new field, the
   command's own among them. A program outside that does the same will notice. Adding a
   field is the rule; it is not free.
3. *It was first built wrong.* The count was kept where a session is read from the log and
   not where a commit is applied while it runs, so it was right after a restart and empty
   otherwise, and every test passed. Two paths bring a session up to date and have to be
   kept in step by hand; the test that catches it compares a run with the same run picked
   up.
4. *Nothing reads it yet.* `Outcome` has no total, and the report still adds up by
   itself.

**Still to decide.** Which requests a total counts. Whether `Outcome` carries it. The unit
a cost is written in, which no count here settles.

## X8. A session that branches from another

*For:* [C14](open-questions.md#c14-can-a-session-branch-from-an-earlier-point). Design
holds that branching can be added as "a new session whose parent is event N of another"
without changing the log. C14 asks for a spike that shows it.

**The form tried.** `Sessions::branch(id, from, at)` (and `Harness::branch_session`) stores
one event: a `session/start` with `{"parent", "from"}`, where `from` is the id of an event
of the parent. The branch's journal begins with what the parent held once that event was
stored, with nothing under way, and then reads its own events. The parent is not touched.
There is no command for it.

**Try it.** `tests/branching.rs`: two answers in a session, a branch after the first, and
each going its own way.

**What building it answered.**

- Design is right about the log: no new topic, no change to a store, one field on
  `session/start`. An older reader sees a child session with an empty conversation.
- A branch of a branch works by the same rule, to a depth the code bounds.
- It is read back from the log by another process as it was.

**What it found.**

1. *Not every event is a place to branch.* After what the person said, the model has not
   answered, and the branch would begin with two messages from the person in a row. After
   the model's tool call, the call has no result, and the branch would run it again in its
   own name. The spike refuses both and allows a branch after the model's answer, after a
   run's end, or before anything was said. Rewinding to "just before this tool call" is
   then not a branch at that event but at the answer before it.
2. *`parent` now means two things:* the session that started this one and waits for it,
   and the session this one branched from. They share the field and so its consequences. A
   branch counts as one level deep for nesting, and when its run ends the harness looks
   whether its parent waits for it.
3. *Removing a session removes its branches,* because removal follows `parent`. That is
   forced: the branch's beginning is the parent's events, and without them it has none.
   The other answer, a branch that keeps a copy, is no longer one event.
4. *A branch reads its parent each time it is opened,* up to the event. Cheap in memory.
   With a store that keeps a file per session it is two files for one conversation, and
   more for a branch of a branch.
5. *Ids in a branch come from two sessions.* A compaction's `keep_from` in the parent
   holds for the branch as well, which is right, and means a branch cannot be read without
   its parent's ids.
6. *What the parent was started with is not copied,* because nothing is recorded there
   yet. When a session keeps a model, grants and limits, a branch has to say whether it
   keeps its parent's.

**Still to decide.** The field's name, and whether a branch is a `parent` or a link of its
own. Where a branch may begin. What a branch keeps of what its parent was started with.
The command, if there is to be one.

## X9. An ask that reaches a person, anywhere

*For:* [C15](open-questions.md#c15-can-a-person-answer-an-ask-at-pluginload), with
[E16](open-questions.md#e16-are-loads-by-a-plugin-durable) and
[G6](open-questions.md#g6-what-can-a-person-answer-to-an-ask). Decided
([Events, gates and hooks](design.md#events-gates-and-hooks)): an ask at any declared point
is recorded and waits in the log for a person, and one `gate/answer` names whichever it
was. Open: what the call that led to the ask does meanwhile.

**Which form, and why this one first.** There are two: the call waits, as a tool call a
gate asked about does, or it fails and is made again once the person has answered. This is
the second. It was chosen to be tried first (28 November 2026) because it is the smaller,
with the first still wanted if a revised design allows it: other harnesses wait here. So
the question is not answered, and what follows is evidence for it.

**The form tried.**

- *In the core,* a trait, `Asks`, and two functions on `Gates`: a host says where asks are
  put (`answered_through`), and whoever got an ask from the gates hands it over
  (`settle`). The core records nothing itself.
- *In the harness,* asks kept in the log (`src/asks.rs`). A gate's ask at `plugin/load` or
  `session/start` is recorded as a `gate/decision` with the verdict `ask` and an `ask` id,
  and what was asked about is refused, with a reason that names the id and says a person
  was asked. `Harness::open_asks` lists the asks that wait, and `Harness::answer_ask`
  records the person's answer as a `gate/answer` naming the id.
- *Asked for again,* the same request finds its answer: allowed, it goes through; refused,
  it fails with the person's reason. Either way a `gate/decision` records that the answer
  was used. An answer is for one asking.
- *An ask's id* is a hash of the point and the request, so "the same" is the same request
  to the letter.
- *Only a host that says so takes answers* (`Harness::answers_asks`). The `corduroy`
  command does not, so for it an ask at these points is still a denial.
- *A tool call of the model's is untouched:* an ask about one still makes the run wait.

**Try it.** `tests/asks.rs`: a gate that asks about every session a sub-agent would start,
and one that asks about every load.

**What building it answered.**

- One mechanism serves both points, and would serve a third. The WebAssembly host and the
  `sessions` service each added a few lines.
- It is all in the log: another process finds the ask, and an answer given there reaches
  the model in this one.
- Only what the host recorded counts. An add-on that raises a `gate/answer` of its own has
  answered nothing, which a test shows.
- The fourth guarantee's missing half, a person approving what the agent loads, can be
  met in this form.

**What it found.**

1. *Allowing something does nothing.* After the person says yes, nothing happens until the
   model asks again, and if its run has ended in the meantime a person has to prompt it
   to. In the tests a second run is needed each time. This is the weight on the other
   side: a call that waits goes on by itself the moment the answer comes.
2. *The retry has to be the same to the letter.* The id is made from the whole request. A
   model that words its task differently the second time is asked again, and so is one
   that starts another child in between, since the request names the child's id. For a
   load the request holds the component's hash, so an add-on rebuilt while the ask waited
   is a new ask; that is the opposite of "the yes is for the grants and not the code",
   which is decided for packages. What an ask's identity is made of has to be said for
   each point.
3. *Nothing stops a model asking again at once.* Each try gets the same refusal, and no
   new record. The refusal tells it to go on with something else; whether a small model
   does has not been tried against one.
4. *`gate/answer` has two shapes now:* `{"id", "tool", …}` for a tool call and
   `{"point", "ask", …}` for the rest. Old logs read as before. What reads answers by a
   call's id (the session's projection, the terminal, the report's "waited on" list) skips
   the new kind, and the report has not been taught to pair an ask with its answer.
5. *The two forms now live side by side.* A tool call the model made waits; a load or a
   session's start fails and is asked for again. A person sees two behaviours for what
   looks like one thing.
6. *The core had to grow for it.* The WebAssembly host and the loop do not know each
   other, and the core reads no JSON, so the meeting point is a trait in `gate`, with the
   log's side in the harness.
7. *Every ask reads every gate event in the log.* Fine for a prototype; an index, or the
   harness keeping the standing asks as it keeps sessions, is what a real one needs.

**Still to decide.** Whether the call waits after all. What an ask's identity is made of.
Whether an answer can be for more than once, and where it is kept (G6). The fields of the
two events. How a person meets an ask: no command, terminal line or page shows one yet,
and their names are H7's.

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

## What waits on a decision

**A loop written as a description**
([E12](open-questions.md#e12-can-an-add-on-ever-contribute-a-loop-step-a-model-or-a-kernel))
was to be tried next, and stops before its first step. Its direction is to write the
test-driven loop as a description and see what it cannot say. The standard loop asks the
model, runs the tools it asked for, and ends when it answers. The test-driven one does one
thing more: when the model says it is done, the loop itself runs the tests, and if they
fail it tells the model so and goes round again. Neither half of that has a form:

- *Telling the model.* The test output has to go into the conversation as something said
  to the model. Today the only one who says things to the model is the person. A message
  from the loop needs to say it is the loop's, so that a review does not take it for the
  person's: that is [B28](open-questions.md#b28-is-there-one-person), which offers a field
  on the message or an event source of its own.
- *Running the tests.* The loop would call a tool the model did not ask for. A loop can
  only run the calls in the model's message today; running one of its own, with the
  record every call leaves, is decided ("a single tool call on `Run`") and not built. And
  its result cannot be recorded as a tool's answer, because a provider refuses an answer
  to a call that was never made.

Both are small to build once chosen. Until then a description has nothing to describe
beyond the standard loop. The pair itself, two agents that differ in loop, tools and view,
also waits on a session being started with those.

**[E1](open-questions.md#e1-what-can-a-host-call-fail-with)** needs no prototype: it is a
decision, and its cost is known (every guest rebuilt, in three languages).
