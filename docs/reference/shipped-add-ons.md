# Shipped add-ons

The add-ons in [`plugins/`](../../plugins), built with `cargo plugins` to
`plugins/target/wasm32-unknown-unknown/release/<name>.wasm` (with `-` as `_`). Every one but
`plugin-manager` is in the default agent (`corduroy init`), which grants each exactly what is
listed here. Configuration goes in the entry's `config`; every key is optional unless marked.

Each crate's documentation (`src/lib.rs`) is the authoritative description.

## pi-tools

The minimal coding toolset, after [Pi](https://github.com/earendil-works/pi), with `tree`
from Goose.

| Tool | Input | Read-only |
| --- | --- | --- |
| `read` | `path`, `offset` (first line, from 1), `limit` | yes |
| `write` | `path`, `content`: create or replace a file; refused if the file changed since these tools last read or wrote it | |
| `edit` | `path`, `edits`: a list of `oldText`/`newText` pairs, each unique in the file, all matched against the original; refused if the file changed since these tools last read or wrote it | |
| `bash` | `command`, `timeout` (seconds): run in the kernel, whose working directory and variables persist | |
| `tree` | `path` (`.`), `depth`: a directory's layout through `fs-read` alone; skips `.git`, `node_modules`, `target`, `.corduroy`, `.venv` | yes |

Output is capped at 2000 lines or 50 KB, and `read`, `bash` and `tree` say so to the harness
as a result of 16 384 tokens at most (counted as three bytes to a token): for a model that
holds little the harness cuts a result to an eighth of its context, the start of a file or
a tree and the end of a command's output, and the result says how much is shown
([Design](../design.md#keeping-within-what-the-model-holds)). A binary file read with `read`, or printed by a
command in `bash`, is not returned: the result names its kind and size instead. A `path`
is relative to the working directory. Config: `kernel`, the kernel `bash` runs in (`shell`);
`tools`, the ones to offer (all five when absent), which is how one is left to another
add-on, as `bash` is to [`lean-shell`](#lean-shell). A name in `tools` that is not one of the
five stops the add-on from starting. Grants: `fs-read`, `fs-write`, `kernel`.

## lean-shell

`bash` in `pi-tools`' place, with each command put through a program that shortens what it
prints before the model reads it: [`rtk`](https://github.com/rtk-ai/rtk) unless `prefix`
names another. Off in the default agent; its entry there is commented out.

| Tool | Input | Read-only |
| --- | --- | --- |
| `bash` | `command`, `timeout` (seconds): as `pi-tools`' `bash` | |

The tool's name, its input and the form of its result (the end of the output, a line when
earlier output was dropped, `[exit status N]`, binary output named by kind and size) are the
same code as `pi-tools`' (`plugins/shell-tool`), so a gate's rule for `bash`, the log and the
model's own habits hold whichever add-on answers. A tool's name belongs to one add-on: an
agent with both lists `lean-shell` after `pi-tools` with `replaces = ["bash"]`, which lets
it take the name over because its `bash` takes what `pi-tools`' takes; without that the
second to offer `bash` does not start
([Replace a shipped tool](../how-to/replace-a-shipped-tool.md)).

By default the model's script is given to `rtk rewrite`, which answers with the script with
`rtk` put in front of the commands it knows, and the answer is run in the shell itself, so
`cd` and variables carry over and a script `rewrite` has nothing for runs as written. The
program is found on the kernel's `PATH`. Where it is not installed the call fails and says
so, and the command is not run another way. The recipe is for a POSIX shell.

| Config | Default | |
| --- | --- | --- |
| `kernel` | `shell` | The kernel commands run in |
| `prefix` | `rtk` | The program, with any arguments it always takes; its first word is what must be installed |
| `run` | ask `{prefix} rewrite`, run the answer | How a script is run: a template holding `{prefix}`, `{quoted}` (the script as one shell word) or `{command}` (the script as written). `"{prefix} {command}"` puts the program in front of whatever was written, which suits single commands only |
| `result_tokens` | `4096` | The most one result takes of the model's context, a quarter of `pi-tools`'; the add-on keeps the last 2000 lines within three bytes a token, and says the same to the harness |

Grants: `kernel`.

## search

Searching the workspace without a shell, after Pi's `grep` and `find`. Both only look, so
`approval` in `smart` mode lets them through, where the same search run in `bash` is a
command it asks about.

| Tool | Input | Read-only |
| --- | --- | --- |
| `grep` | `pattern` (a regular expression, matched against each line), `path` (a directory, searched all the way down, or one file; `.`), `glob` (only files matching it), `ignore_case`, `context` (lines around each match), `limit` (most matching lines, 100), `files_only` | yes |
| `find` | `pattern` (on the path: `*.rs` is that name at any depth, `src/**/*.toml`, `*.{ts,tsx}`; `*` stays within one name, `**` crosses directories, `?` is one character; a set of characters such as `[0-9]` is not supported and is refused, saying so), `path` (`.`), `limit` (200) | yes |

`grep` answers as `path:line:text`, with `--` between runs of context. Both pass over `.git`,
`node_modules`, `target`, `.corduroy` and `.venv`; `grep` also passes over files that are
not text or that the grant does not reach, and the agent file in the working directory. A
result is capped at 50 KB and 20 000 files, and at less for a model that holds little, as
`read` is: the first matches are the ones kept.

Config: `max_bytes`, the most text one `grep` reads (16 MB). A call into an add-on is
metered, so a search stops there and says so; the model narrows it. Grants: `fs-read`.

## fetch

A page on the web, as text.

Tool: `web_fetch` (`url`, a whole `http` or `https` address; `offset`, bytes of the text to
pass over; `raw`, the HTML itself). HTML comes back as text with its headings, lists and
links, the links as `[words](address)` worked out against the page; text and JSON as they
are; anything else as its kind and size. An answer of 400 or more is an error with its
status. Scripts are not run, so a page built by them has little to read. One call returns at
most `max_bytes` and says the `offset` to read on from.

The host's client follows no redirect. The add-on follows up to five itself, each a request
of its own, so a redirect to a host outside the grant is refused like any request there.

It is **not** marked read-only: an address is something sent out, and a model that has read
a hostile page can be told to put what it knows in one. So `approval` in `smart` mode asks
about each fetch. Add `web_fetch` to its `allow` list to stop that, or keep the grant to the
hosts you mean (`http = ["docs.rs", "github.com"]`). Every request is on the log as
`http/request` and `http/response`.

Config: `max_bytes` (50 000), `user_agent`. Grants: `http`, by host; the default agent
grants every host.

## skills

Skills the agent writes for itself, after Hermes: `<dir>/<name>/SKILL.md` with `name` and
`description` front matter. Only an index goes in the system prompt.

Tools: `skills_list`, `skill_view` (read-only), `skill_manage` (`create`; `patch`, either one
unique string or, with `content`, the whole file; `delete`). After `nudge_interval` tool calls without a skill being saved, it emits
`skills/review-due` at the end of the turn and reminds the model until one is. Emits
`skills/changed` on every change.

Config: `dir` (`skills`), `nudge_interval` (10). Grants: `fs-read` and `fs-write` on `dir`,
`events` on `skills/*`.

## approval

A gate at `tool/call`: permission modes, after Goose.

| `mode` | Read-only tools | Other tools |
| --- | --- | --- |
| `auto` | run | run |
| `smart` (default) | run | ask a person |
| `approve` | ask | ask |
| `chat` | refused | refused |

Config: `mode`, `allow` (tool names that always run), `deny` (tool names that never run;
wins over `allow`). Names are matched exactly. Grants: none.

## inspect

A gate at `tool/call` and a reader of tool results, after Goose's inspectors.

| Finding | Then |
| --- | --- |
| a command that destroys a lot at once (`rm -rf /`, `git push --force`, `curl … \| sh`, …) | ask |
| a credential in the input (a private key; an AWS, GitHub, Slack or Anthropic key) | ask |
| the same call with the same input `max_repeats` times in a row | deny |
| a tool result that tries to instruct the model ("ignore previous instructions") | a caution in the system prompt for the rest of the turn |

Config: `max_repeats` (3), `ask` and `deny` (extra substrings that make it ask or refuse).
Grants: none.

## hints

Context files in the system prompt, read again before every request.

Config: `files` (`AGENTS.md`, `.goosehints`, `CLAUDE.md`; relative paths are in the
workspace), `max_bytes` (32 KiB in all). Grants: `fs-read` on those files; a file the grant
does not reach is skipped.

## today

Today's date in the system prompt, read from the host's clock. A model otherwise takes the
date from its training, and is a year or two behind.

Tool: `now` (the date and the time; read-only). Config: `utc_offset`, how far your clock is
from UTC, as `+01:00` or `-08:00`. An add-on cannot see the machine's time zone, so without
it dates and times are UTC, and say so. Grants: `clock` on `wall`.

## todo

A todo list per session, in the system prompt while it is not empty.

Tools: `todo_read` (read-only), `todo_write` (replaces the whole list). Grants: `state`,
`session`.

## memory

Memories by category, one Markdown file per category, one entry per paragraph, so you can
read and edit them. What is saved goes in the system prompt.

Tools: `memory_save`, `memory_recall` (read-only), `memory_forget`. Config: `dir` (`memory`),
`prompt_bytes` (8 KiB). Grants: `fs-read`, `fs-write` on `dir`.

## recall

Search earlier conversations in the log.

Tools: `recall_sessions`, `recall_search` (both read-only). Grants: `log-read` on
`*:message` and `*:session/start`; narrow the session part to keep sessions out of reach.

## summarize

Files and directories summarised by a model of its own, so the main model reads less.

Tool: `summarize` (`paths`, `question`; read-only). Config: `model` (required, as
`<provider>/<model>`; the `llm` grant must reach it), `max_bytes` (200 KB). Grants: `fs-read`,
`llm`.

## review

A second model reviews the change in the workspace: what `git diff <base>` shows, and the
files git does not track yet, sent once through `llm` with what the change is meant to do.
Its findings are the tool's result, for the model in the loop to fix. The reviewer has no
tools of its own; it sees what it is sent. In the agent `corduroy init` writes it is the
other vendor's model when both `ANTHROPIC_API_KEY` and `OPENAI_API_KEY` are set where `init`
is run, else the same model.

Tool: `review` (`goal`, `base`, `paths`: more files to read whole; read-only). With no
change and no `paths`, the reviewer is not asked. Config: `model` (required, as
`<provider>/<model>`; the `llm` grant must reach it), `kernel` (`shell`), `dir` (the repository; the kernel's
directory by default), `base` (`HEAD`), `max_bytes` (200 KB), `instructions` (more for the
reviewer to look for). Grants: `kernel`, `fs-read`, `llm`.

## compact

Auto-compaction: provides the `compaction` service the harness asks before each step. When a
request passes `max_tokens` input tokens, or the conversation passes `max_messages`, the
start of it is summarised; the messages stay in the log, hidden from the model. The
harness chooses where to cut: where the turn began, or, while a turn is under way, before
its last step, so one long run is summarised as it goes. A session whose last run ended
because its request was more than the model holds is summarised before its next request
whatever the counts say. What is sent to the summariser is held to half of what the
session's model holds (the start and more of the end of the transcript), so the request to
summarise is not itself too much.

The count compared with `max_tokens` is what the next request is expected to take: what
the last one took and was answered with, and an estimate of what was added since.

Config: `model` (required, as `<provider>/<model>`), `max_tokens` (three quarters of what the
session's model holds, as its [description](agent-file.md#providersnamemodelsmodel) says;
100 000 if it does not say), `max_messages` (off). Grants: `llm`.

## subagents

Tool: `delegate`: start a child session for a task, with the same model, tools and gates and
an empty conversation. The parent's run waits, in the log, until the child ends; the child's
answer is the call's result. Grants: `service` on `sessions`.

## viewer

The log as a page in a browser, as files: `<dir>/index.html`. An add-on cannot listen on a
port, so nothing is served: the page is files, opened as files, and it follows a run as it
goes by loading them again every two seconds. The [`page` extension](agent-file.md#extensionsname)
serves the same page while `corduroy` runs, where a gate's question can be answered; this
one is there when it does not.

| View | Shows |
| --- | --- |
| a session | what you said and the agent answered; each tool call with its input, its result, how long it took, and what a gate asked or refused and what you answered; each request to the model with its time and tokens; how each run ended. A sub-agent's session is linked from the call that started it |
| Problems | failed runs, tools and model calls, what a gate refused, calls outside a grant, add-ons that failed, trapped or were stopped, runs cut short and turn limits, each linked to where it happened |
| What it has and used | what wrote the log, the plugins started (and those that did not, with why), the models asked with their tokens, the tools called with how often they failed |
| Every event | the log as `corduroy log` prints it, filtered as you type |

It only shows, and says so along its foot, with how to turn on the page that answers.
While a gate's question waits, a bar over every view says which session waits, about what,
and where to answer: in the terminal, or on the page the `page` extension serves. A page that takes an answer listens on a socket, and so is an extension
([How people meet it](../design.md#how-people-meet-it)).

- **It holds a copy of the log**, a payload over 256 KB cut short. Whatever can read `dir`
  can read what was said in every session. Set `enabled = false` if nothing should hold one.
- **A removed session leaves it at the next start.** After `corduroy sessions remove`, the
  copy still holds the session until an agent with the add-on starts in that workspace;
  then all of it is written again without it. The same goes for a session a narrowed
  `log-read` grant no longer reaches. Delete `dir` to have it gone at once.
- **The page trusts nothing it shows.** What a model or a tool wrote is put on it as text,
  and its content policy lets it load only the files beside it and reach nothing else. The
  page's own files are written again at every start.

Config: `dir` (`viewer`, in the
workspace; the default agent uses `{data}/viewer`). Grants: `log-read` on everything it
should show (`"*"`, or narrower to keep sessions or topics off the page), `fs-read` and
`fs-write` on `dir`, `log`.

## plugin-manager

Lets the model find, load and unload add-ons while it runs. Each load is asked of the gates
at `plugin/load`, and each grant is narrowed to what `plugin-manager` itself holds. Off in the
default agent (`enabled = false`).

Tools: `plugin_search`, `plugin_list`, `plugin_load`, `plugin_unload`. Config: `dirs`, where
to look. Grants: `fs-read` on `dirs`, `plugins`, and whatever it should be able to pass on.

## Examples and test plugins

| Where | What |
| --- | --- |
| [`examples/notes-js`](../../examples/notes-js/plugin.js), [`examples/notes-py`](../../examples/notes-py/notes.py) | the same notes plugin in JavaScript and Python (`note_add`, `note_list`, `note_remove`), built by `scripts/guests.sh` to `notes_js.wasm` and `notes_py.wasm`; grants `state`, `log`, `clock`, `random` |
| [`sdk/js/script-host`](../../sdk/js/script-host) | one interpreter component that runs automations written as scripts, each with its own grants; see [The script host](script-host.md) |
| [`plugins/testbed`](../../plugins/testbed) | for the host's own tests only |
