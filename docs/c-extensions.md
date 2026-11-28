# Extensions through C: what a prototype found

[G23](open-questions.md#g23-can-an-extension-be-written-in-a-language-other-than-rust) asks
whether an extension can be written in a language other than Rust. Nothing is decided. This
page is the record of a prototype built to find out what the question is made of:
`crates/corduroy-cabi`, with a header (`include/corduroy.h`), an extension written in C
(`examples/c/notes.c`) and tests that run it (`tests/c_extension.rs`). **None of it is
promised, shipped in the command or reachable from an agent file.** When G23 is decided, this
page is deleted and what holds is written where it belongs.

To run it (needs a C compiler; without one the tests are left out, and with
`CORDUROY_REQUIRE_C` set that is an error):

```bash
CORDUROY_REQUIRE_C=1 cargo nextest run -p corduroy-cabi --all-features
```

**Who it is for.** Systems languages and the languages of a platform (C, C++, Zig, Swift,
Go, C#): a way to write an extension against a platform's own interfaces without writing
Rust. Not scripting languages, which stay add-ons. That is the aim the prototype is read
against below; it is an aim, not yet a decision of G23.

## What was built

A foreign library exports one function. It fills in a table: a name, the services it
uses, an `apply` and a `drop`. `CExtension` wraps that table as an ordinary
`corduroy_core::Plugin`. When the plugin is applied, the foreign `apply` is handed a table
of the host's functions and a context, and registers through them. Each registration goes
through the plugin's own `Context`, so it is an effect and unloading takes it back.

| Carried | How it crosses |
| --- | --- |
| Tool, service, gate, prompt part, secret | a function pointer and the extension's own pointer; text in, a status and text out |
| Listener, effect | a function pointer called with the event, or at unloading |
| Emit an event, call another plugin's service | host functions on a handle the extension keeps after `apply` |
| Whether the run was asked to stop | `host->stopping()`, the thread's call scope |
| Model provider (the `harness` feature) | a request as JSON and a *completion*; the extension returns and completes later, from any thread |

The C extension contributes one of each, the loop runs a whole turn against a model
written in C that answers from a thread of its own, and unloading leaves nothing behind.

It gets in both ways G23 names. Linked when the program is built, `CExtension::load` is
given the entry function. Loaded when the program runs (the `load` feature, tried on Unix),
`CExtension::open` is given a path and the function's name. The same library, built from
the same source against the same header, serves both.

Six rules make the boundary, and they are at the top of the header. The two that shaped
everything: **nothing is freed across it** (text is borrowed for one call; whoever produces
text hands it to a function of whoever wants it, which copies), and **the extension calls
the host only through the table it is handed**, so a library built against the header has
no undefined symbols and can be linked in or loaded without the interface changing.

## What it settles, as far as a prototype can

- **The core did not change.** Not a line. The mirror is a crate beside it, and an
  extension like any other. A provider is registered by loading the existing `provider`
  plugin as a child, through `Context::plugin`.
- **Asynchrony needs no extension of its own, and no second interface.** A completion is a
  pointer the host makes and a function that fulfils it. On the Rust side that is a future
  holding a waker: no runtime is shared, named or needed, which is how the loop already
  waits ([Without the standard library](design.md#without-the-standard-library)). A
  synchronous answer is the same call completed before it returns. So the interface has one
  form, and the loop's half of it is a part of the table that is null in a host built
  without the loop.
- **A registration that fails can fail `apply` without the C author checking.** The host
  notes the first failure; `apply` fails with it whatever the extension returns.

## What it found

These are the questions the prototype raised or sharpened. They are the brief for whoever
decides G23.

**The crossing**

1. *Loading moves the `unsafe` declaration inside, and leaves the trust outside.* Linked
   at build time, whoever assembles the command writes `unsafe extern "C" { fn
   corduroy_extension_notes(..); }`, and the workspace forbids that. Loaded, this crate
   finds the symbol and the caller declares nothing. But `open` is still an `unsafe fn`,
   and rightly: a wrong file corrupts the process. So a command that forbids `unsafe` can
   use neither as they are. For it to load an extension, this crate would have to offer a
   safe function, which is this crate vouching for a file it has never seen. Whether that
   is acceptable, on the grounds that naming the file in the right place is the person's
   act of trust, is the real form of G23's second decision.
   *A library is never unloaded.* A function of it may still be running or held when its
   extension goes, so the prototype keeps every library for the life of the program.
   Unloading an extension takes back what it registered and leaves its code mapped: for an
   extension that was loaded, "unplugged again" means less than it does for an add-on.
2. *`forbid` cannot be lifted for one crate.* Cargo cannot inherit the workspace's lint
   table and change one entry, so the prototype carries a copy of the whole table with
   `unsafe_code = "deny"`. The copy will drift.
3. *Four ways to corrupt the process that the host could close and does not.* A context
   used after `apply`; a handle used after `drop`; a completion completed twice; a function
   pointer called after its extension is unloaded while a call is in flight. Each can be
   made a refusal at the cost of a table of live handles and a check per call. Which are
   closed and which are promised by the author is undecided.
4. *A fault is the process's.* A trap in an add-on ends one instance and it is restarted.
   Here a segfault, a `longjmp` or a C++ exception through a callback ends the command,
   and nothing can be caught. A panic on the Rust side aborts.
5. *Every callback may arrive on any thread.* That is what `Send + Sync` means once only a
   promise is left. For the languages this is for, a call that blocks is enough, and the
   prototype's shape stands; an interpreter with one thread could not be called this way,
   which is one more reason scripting languages stay add-ons. What the aim does bring is
   the platform's own threading: much of a platform's interface must be called on one
   particular thread (the main thread, for most of macOS's and Windows's), or answers
   through a callback of its own. A tool written in Swift that must hop to the main thread
   and wait is the case to try next, and it may want the completion form for tools as
   well as for providers.

**What does not cross yet, and why**

6. *A request and a reply have no text form.* The prototype made one up around the
   `message` event's JSON. Add-ons get records from the WIT world; the log writes messages
   one at a time. A C interface makes "the JSON of a `ModelRequest`" a contract, and it is
   not obvious that this crate is where it should be defined.
7. *What a provider tells on the way does not cross.* Pieces of a streamed answer and
   retries (`Reports`) have no callback here, so a provider across C cannot keep the rule
   that a request made again is told. Nor is it told when the loop stops waiting: the
   future is dropped and the extension completes into nothing.
8. *A tool's result is text only.* Images, the result budget (`ToolSpec::result`,
   `call::budget`), the session a call serves and `call::wait_for` are each one more host
   function. None is hard. Together they are the real cost: **every addition to the core
   is three edits here** (the Rust mirror, the header, the code that crosses), and the
   header is written by hand. Generating it is untried.
9. *`Operation`, `Loop`, `Kernel` and `Front` were not attempted.* An operation reads a
   `&Session` and a loop is handed a `&Run`: borrowed views with methods, not data. Over C
   each becomes a table of host functions of its own, or the session serialised whole on
   every step. A kernel holds pipes.
10. *An extension's own service is a Rust type.* `Context::provide` and `Context::service`
    hand out an `Arc<T>` found by type. The prototype reaches `Tools`, `Gates`, `Prompts`,
    `Secrets`, `Services` and `Providers` because it has a hand-written function for each.
    A registry some other extension provides (the harness's `Operations`, `Loops`, `Fronts`,
    `Outside`, or a third party's) is invisible to C until someone writes its mirror. Only
    what is in `Services` crosses without new code, because only that is called in text.
11. *Blocking work has nowhere to go.* A provider that blocks (the simplest thing to write
    in C) would hold the executor. The harness's `tools::blocking` is private to it.
12. *The growth rule is laid out and half built.* Every table starts with its size and
    grows at its end, but the host refuses a table shorter than its own, so an extension
    built against an older header would not load. Reading a prefix and defaulting the rest
    is the missing half.
13. *The prototype needs `std`,* for its lock and for text. A device is where C is most
    likely to be wanted. Whether the mirror can be `no_std` over the core's own lock is
    untried.
14. *The other direction is untouched:* a program in another language that embeds the
    harness and drives runs (G23's fifth decision).

## What it says about the core

The design already says what the core is
([The core boundary](design.md#the-core-boundary)): a path every effect passes through, a
kind of thing plugins contribute, or something plugins must agree on to work together.
The prototype is evidence for that boundary more than against it. A whole second way of
writing extensions fitted on top without the core knowing.

It also shows where the core is easy to build on and where it is not. What crossed for
free is what the core already speaks in text: `Tool::call`, `Service::call`, `Gate::check`,
prompt parts, secrets, events. What needed inventing or was left out is what the core and
the loop speak in Rust types: a model request, a tool's description and content blocks, a
session, a typed service. Text is the core's lowest common shape, and it is where
WebAssembly, C and the log already meet. So the question the prototype puts to the core is
not "should it be C" or "should it be asynchronous" but: **should each thing plugins must
agree on have one text form that the core, or one crate beside it, owns?** Today the
`message` JSON is the harness's, the request's is nobody's, and the WIT records are a
third shape of the same things.
