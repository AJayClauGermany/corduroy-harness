# Trials: things to try, and what to say about them

Some of the [open questions](open-questions.md) are easier to answer after using a thing
than before. Where the code can offer both answers at once, it does, and this page says how
to try each. A trial is a question, the ways to try it with the exact commands, what to
look at, and a line for what you found.

**To give feedback, write on the *Found* line** and commit the page, or just say it by its
number ("T2: B, `login` reads wrong for an API key"). A trial that has its answer is deleted
from this page: the answer is written where it belongs ([Design](design.md), the
[reference](README.md#reference)), the way that lost is taken out of the code, and the open
question it hangs off is updated or removed.

Nothing here is a promise. The names and defaults on trial are the ones marked as not final
in [How people meet it](design.md#how-people-meet-it).

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

**What has only been tried on Linux.** The keychain was run for real against the Secret
Service (GNOME Keyring) on Linux. For macOS and Windows it was compiled and linted, and the
logic above the system's own store is tested everywhere against a keychain in memory, but
nothing here has run against a real macOS Keychain or Windows Credential Manager until CI
or you do. T0 is that run.

---

## T0. Does the keychain work on your machine at all?

*Answers:* whether one real round trip is enough of a test for the keychain kinds
([H10](open-questions.md#h10-which-platforms-does-10-promise)).

```bash
CORDUROY_REQUIRE_KEYCHAIN=1 cargo nextest run -p corduroy-keychain
```

Without the variable the test does nothing: a test run never touches your keychain unasked.
With it, it keeps a throwaway item (`corduroy-test-<pid>`, service `corduroy`), reads it
back, replaces it with a value longer than one Windows credential holds, and removes it.

*Look at:* does it pass; does macOS ask anything, and how often; is anything left behind
in Keychain Access (search `corduroy`).

> Found:

## T1. Should `init` put keys in the keychain?

*Answers:* whether the decided default (the keychain wherever there is one) is livable as
the first thing a new person meets. It costs one more command than an exported variable.

**A. The keychain** (what `init` does where there is one):

```bash
corduroy init anthropic
```

```bash
printenv ANTHROPIC_API_KEY | corduroy secrets set anthropic
```

```bash
corduroy doctor
```

**B. The environment, written down:**

```bash
corduroy init anthropic --secrets env
```

```bash
corduroy doctor
```

*Look at:* how many steps to a model that answers; whether what `init` printed was enough
to get there without this page; on macOS, how often the Keychain asks to let `corduroy` in
(a build from a checkout is a new program after every rebuild, so "Always Allow" does not
last; an [installed build](how-to/install-a-build.md) should be asked once); what a run
says when the key is not there yet (try `corduroy run hi` before `secrets set`).

> Found:

## T2. What is the command that puts a value in called?

*Answers:* the command's name
([H7](open-questions.md#h7-are-the-agent-files-older-names-final)). Both exist, and do the
same thing for a `kind = "keychain"` secret.

**A.** `corduroy secrets set anthropic`, with `corduroy secrets remove anthropic`, beside
`corduroy secrets`, which lists.

**B.** `corduroy login anthropic`, with `corduroy logout anthropic`: the same words as for
a ChatGPT plan, so there is one thing to remember for "this secret needs me".

Try each typed at the prompt (nothing shows as you type or paste), and piped:

```bash
printenv ANTHROPIC_API_KEY | corduroy login anthropic
```

*Look at:* which you reached for; whether `login` reads right for an API key; whether
leaving the name out (allowed when the file has one such secret) helps or hides what is
being set.

> Found:

## T3. Where is a ChatGPT sign-in kept?

*Answers:* whether `keep` should exist as a key at all, or the keychain should simply be
where a sign-in goes when there is one
([H7](open-questions.md#h7-are-the-agent-files-older-names-final)). Today a `chatgpt`
secret without `keep` is kept in a file, as before, so that an agent file written earlier
behaves as it did.

**A. A file of your own** (`~/.config/corduroy/chatgpt/<name>.json`, readable by you alone):

```bash
corduroy init chatgpt --secrets env
```

```bash
corduroy login
```

**B. The keychain.** In the same `corduroy.toml`, add `keep = "keychain"` under
`[secrets.chatgpt]`, then:

```bash
corduroy login            # moves the sign-in out of the file; no browser this time
```

```bash
corduroy secrets          # says who is signed in, where it is kept, how long the token is good
```

(`corduroy init chatgpt` writes B from the start where there is a keychain.)

*Look at:* that the file is gone after the move and runs still work; that a run an hour
later still works (the token is renewed and written back to the keychain); on macOS,
whether the Keychain asks again at each renewal; on Windows, that it works at all (the
sign-in is longer than one credential holds and is kept as numbered pieces).

> Found:

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
else wrote included, wherever its provider's `base_url` points. Would you rather say, when
you put a key in, which hosts it may go to?

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

## T6. Is `doctor` a command of its own?

*Answers:* its name and what it covers
([H7](open-questions.md#h7-are-the-agent-files-older-names-final)). `check` loads the agent
and asks nothing of the outside; `doctor` also tries each key, kernel and model.

```bash
corduroy check
```

```bash
corduroy doctor
```

Break something on purpose and run both again: `corduroy secrets remove anthropic`, a
wrong `model = "..."`, `corduroy logout`.

*Look at:* whether each failure's line told you what to do; whether `doctor` should be
`check --live`, or `check` should do all of it; whether asking each model a question (a
few tokens, a second or two) is right for something you run often; what else it should
try (an MCP server's tools, a file written in the workspace).

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

*Answers:* [G17](open-questions.md#g17-where-is-usage-added-up). `corduroy report` now has a
`used` line for each run, added up by the report from the `usage` events: the reader's sum,
with nothing new in the log.

```bash
corduroy run "say hi" && corduroy report
```

*Look at:* whether a sum made when you ask is enough, or you want the total somewhere a
program reads without adding (on the run's end, in the log); whether it should count the
models add-ons ask (`summarize`, `review`, `compact`), which it does not today.

> Found:
