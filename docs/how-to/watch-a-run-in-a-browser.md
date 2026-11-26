# Watch a run in a browser

The default agent writes its log as a page. Open it:

```bash
open .corduroy/viewer/index.html        # macOS
xdg-open .corduroy/viewer/index.html    # Linux
start .corduroy\viewer\index.html       # Windows
```

Leave it open beside the terminal. It reads the files beside it again every two seconds, so
a run shows as it goes: each tool call, what a gate asked, how long the model took.

The page is there once the agent has started in the workspace at all: `corduroy check` is
enough. During the first run in a workspace, the agent says where:

```text
  [plugin:viewer] open /home/you/project/.corduroy/viewer/index.html in a browser to see what the agent does
```

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

## What the page cannot do

It shows; it does not answer. When a gate asks, a bar across the top of every view says
which session waits, about which call, and where to answer; the tab's name begins
`(asks you)`. You answer in the terminal (`corduroy continue` if the run is no longer in
one). You also type
prompts there. A page that takes an answer has to be served, and what serves is an
extension compiled into the command, which is planned and not built
([How people meet it](../design.md#how-people-meet-it)).

## Put it elsewhere, show less, or turn it off

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

The directory holds a copy of the log. A session you remove with `corduroy sessions remove`,
or that a narrowed `log-read` no longer reaches, leaves the page the next time the agent
starts; delete the directory to have it gone at once. See [viewer](../reference/shipped-add-ons.md#viewer).
