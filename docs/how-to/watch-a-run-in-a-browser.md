# Watch a run in a browser, and answer it there

Two things put the agent's record in a browser. The `viewer` add-on, which the default
agent has, writes it as files you open: it shows. The `page` extension, which you turn on,
serves the same page while `corduroy` runs: on it you can also answer a gate, type a prompt
and stop a run.

## Turn the page on

In `corduroy.toml`, take the `#` off these two lines (`corduroy init` writes them commented
out), or add them:

```toml
[extensions.page]
kind = "page"
```

`corduroy` then says where the page is each time it is about to run the agent:

```text
the agent's page is at http://127.0.0.1:41873/8f3c…/ (a gate's question can be answered there as well as here; the address is new each time, and lets in whoever has it)
```

Open that address. The page shows each session as it goes: what you said and the agent
answered, each tool call with its input and result and how long it took, each request to
the model with its tokens.

## Answer a gate on the page

When a gate asks about a tool call, a bar across the top of every view says which session
waits and about what, and the tab's name begins `(asks you)`.

- **Allow** runs the call.
- **Refuse** refuses it. What you type in the box beside it first is the reason the model
  is given; Enter in the box refuses too.

The terminal asks the same question at the same time. Whichever answer comes first is the
answer; the terminal then says `✓ allowed on page` or `✗ refused on page: …` and goes on.

## Type a prompt, and stop a run

Under a session is a box. In a conversation (`corduroy` with no command), what you type in
it and send with Enter is a prompt, as if typed at the terminal, where it shows as
`› … (typed on page)`. The terminal still takes prompts too.

- The box takes a prompt only while the session is doing nothing. While a run goes it is
  greyed, and **Stop** is beside it.
- **Stop** ends the session's run, as Ctrl-C at the terminal does: the tool that is
  running is told, a question that waits is dropped, and the session takes a new prompt.
- `corduroy run "…"` runs one prompt and leaves, so its page takes no prompts and says so.
  Its run can still be answered and stopped there.
- A sub-agent's session has no box: it is spoken to by the agent that started it.

## Find what went wrong

**Problems**, in the list on the left, gathers what failed across every session: runs,
tool calls, model calls, add-ons that did not start, calls outside a grant, what a gate
refused. Each line links to where it happened in its session.

**What it has and used** says which plugins started and which did not, with why, which
models were asked and for how many tokens, and which tools were called and how often they
failed.

**Every event** is `corduroy log` with a filter: type a topic (`gate/decision`), a session,
a tool's name or a word of a payload.

To hand a problem to someone else, use [`corduroy report`](report-a-problem.md): the page
is for looking, the report for sending.

## Who can open it

Whoever has the address, on this machine. It is served on `127.0.0.1` only, the address
holds a token that is new every time `corduroy` starts, and a request without it is
refused. The address is shown to you and never written to the log, since the log is
something the agent's own tools can read, and an agent that could read it could answer its
own questions.

So an address from an earlier run stops working: open the one `corduroy` says now. A page
left open from an earlier run says so when you try to answer on it.

## Keep the port

```toml
[extensions.page]
kind = "page"
port = 4747        # the same port every time; without it, any free one
```

The token still changes with every start.

## When corduroy is not running

The `viewer` add-on writes the same page as files, so the record can be read with nothing
running, and with no page turned on:

```bash
open .corduroy/viewer/index.html        # macOS
xdg-open .corduroy/viewer/index.html    # Linux
start .corduroy\viewer\index.html       # Windows
```

Opened this way the page shows and cannot be answered on: a run that waits says so, and
`corduroy continue` asks its question again. The directory holds a copy of the log:

```toml
[[plugins]]
name = "viewer"
path = "viewer.wasm"
config = { dir = "{data}/viewer" }
[plugins.grants]
log-read = "*"
fs-read = ["{data}/viewer", "{data}/viewer/*"]
fs-write = ["{data}/viewer", "{data}/viewer/*"]
log = "*"
```

- **Elsewhere:** change `dir` and the two `fs-` grants together, for example to
  `{home}/.corduroy-pages/my-project`, outside the workspace and so out of reach of the
  agent's own file tools.
- **Less:** narrow `log-read`, which is scoped by `<session>:<topic>`. `"main:*"` shows the
  session `main` only; `["*:message", "*:turn/end"]` shows what was said and how runs ended,
  and no tool call.
- **Off:** `enabled = false`.

A session you remove with `corduroy sessions remove`, or that a narrowed `log-read` no
longer reaches, leaves these files the next time the agent starts; delete the directory to
have it gone at once. See [viewer](../reference/shipped-add-ons.md#viewer).
