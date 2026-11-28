# Trials: things to try, and what to say about them

Some of the [open questions](open-questions.md) are easier to answer after using a thing
than before. Where the code can offer both answers at once, it does, and this page says how
to try each. A trial is a question, the ways to try it with the exact commands, what to
look at, and a line for what you found.

**To give feedback, write on the *Found* line** and commit the page, or just say it by its
number ("T5: every request, I rotate keys while a chat is open"). A trial that has its answer is deleted
from this page: the answer is written where it belongs ([Design](design.md), the
[reference](README.md#reference)), the way that lost is taken out of the code, and the open
question it hangs off is updated or removed.

Nothing here is a promise. The names and defaults on trial are the ones marked as not final
in [How people meet it](design.md#how-people-meet-it). A trial keeps its number, so a gap is
one that was answered: T1 (`init` puts keys in the keychain, and what it prints is enough),
T2 (`secrets set` puts a value in; `login` is for what is signed in to), T3 (a sign-in is
kept in the keychain unless its entry says `keep = "file"`) and T6 (`doctor` is a command
of its own, under that name).

## Before you start

A scratch folder keeps the trials away from real work:

```bash
mkdir -p ~/corduroy-trials && cd ~/corduroy-trials
```

From a checkout, `corduroy` below is `cargo corduroy --workspace ~/corduroy-trials` (run
`cargo plugins` once first). To start a trial over, remove `corduroy.toml`.

Two commands answer "is it working" for every trial:

```bash
corduroy secrets          # is each key or sign-in there? nothing is read out
```

```bash
corduroy doctor           # each key, each kernel, one short question to each model
```

Both are safe to paste into feedback: neither prints a secret.

**Where it has been run.** For real against the Secret Service (GNOME Keyring) on Linux
and, by hand, against the macOS Keychain: `init`, `secrets set`, `doctor`, a run, and a
ChatGPT sign-in moved into the keychain. CI keeps, reads and removes an item in the real
keychain on Linux, macOS and Windows. The command itself has not been used on Windows.

---

## T0. Does macOS ask after a rebuild?

*Answers:* how a build from a checkout lives with the Keychain, which decides what the
how-to says and whether reading a secret once for a run is enough
([T5](#t5-read-once-for-a-run-or-for-every-request)). The Keychain lets the program that
put an item in read it back unasked. A build made again after a change should be, to the
Keychain, another program, and be asked: that is the part nobody has seen yet.

Put a key in with one build, change anything in the code, build again, and read it:

```bash
cargo build -p corduroy-cli && corduroy secrets
```

*Look at:* whether macOS asks to let `corduroy` use the item; whether "Always Allow" lasts
until the next rebuild; whether an [installed build](how-to/install-a-build.md) is asked at
all after an update.

> Found (26 November 2026, macOS, one build made on the machine and not rebuilt): no
> prompt at any point, for a key or for a sign-in. Not yet tried across a rebuild.

## T4. Is a secret's name the right name for its item?

*Answers:* how an agent file says which item it means, and part of
[B9](open-questions.md#b9-how-does-a-secret-say-where-it-may-be-sent). An item is kept
under the entry's name and nothing else, so it is yours across every workspace.

```bash
mkdir -p ~/corduroy-trials-2 && cd ~/corduroy-trials-2 && corduroy init anthropic
```

```bash
corduroy secrets          # already there: the first workspace put it in
```

*Look at:* whether sharing is what you want, or a surprise; what you would do for a second
key (a work account): today that is a second name, `[secrets.work-anthropic]`. And the
other side of sharing: any agent file that names `anthropic` gets the key, a file someone
else wrote included, wherever its provider's `base_url` points. That part is answered: the
hosts a key may go to are said when it is put in, and kept beside it
([What is safe by default](design.md#what-is-safe-by-default)). One form of it can be
tried, as a [prototype](prototypes.md#x1-a-secret-names-the-hosts-it-may-go-to):
`corduroy secrets set anthropic --hosts api.anthropic.com`, after which a file that points
the provider elsewhere is refused the key. A key put in without `--hosts` still follows
the file.

> Found:

## T5. Read once for a run, or for every request?

*Answers:* what `secret` promises about a value that changes
([agent file](reference/agent-file.md#secretsname)). A secret from the environment is read
for every request. One from the keychain is read once and kept until the command exits,
because a keychain may ask you each time it is read.

Start a chat (`corduroy`), then in another terminal put a wrong key in
(`echo wrong | corduroy secrets set anthropic`) and send another prompt in the chat.

*Look at:* the chat keeps working on the key it read. Is that what you expect, or should a
changed key be picked up, at the cost of the keychain being read (and perhaps asking) for
every request?

> Found:

## T7. A token for an MCP server

*Answers:* the key's name on an MCP entry
([H7](open-questions.md#h7-are-the-agent-files-older-names-final)).

**A.** `secret = "docs-mcp"` with a `[secrets.docs-mcp]` entry (`kind = "keychain"`, or
`env`). **B.** `bearer_token_env = "DOCS_MCP_TOKEN"`, as before.

See [Add an MCP server](how-to/add-an-mcp-server.md) for the entry.

*Look at:* the token is read once, when the server is connected to, so a missing one stops
the server from starting (`corduroy check` says why) and does not fail a call later. Right
that way round?

> Found:

## T8. Who adds up what a run used?

*Answers:* [G17](open-questions.md#g17-where-is-usage-added-up).

**A.** `corduroy report` has a `used` line for each run, added up by the report from the
`usage` events: the reader's sum, with nothing new in the log.

```bash
corduroy run "say hi" && corduroy report
```

**B.** The run's end holds the total: `used` on its `turn/end`, kept by the harness
([a prototype](prototypes.md#x7-what-a-run-used-on-its-end)).

```bash
corduroy log | grep turn/end
```

The two differ on purpose where a run started sub-agents: the report's sum has what they
used, and a `turn/end` has its own session's requests only.

*Look at:* whether a sum made when you ask is enough, or you want the total somewhere a
program reads without adding (on the run's end, in the log); whether it should count the
models add-ons ask (`summarize`, `review`, `compact`), which it does not today.

> Found:
