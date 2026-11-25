# Have a second model review the work

One model writes the change; another, ideally from another vendor, reviews it before the work
is called done. There are two ways today: the `review` add-on, where the reviewer is asked
once from inside the run, and two sessions, where the reviewer is a whole agent of its own.

## With both keys: `corduroy init` sets it up

```bash
export ANTHROPIC_API_KEY=...
export OPENAI_API_KEY=...
corduroy init
```

The agent file it writes runs Claude in the loop and gives it the `review` tool, which asks
OpenAI's model through a second provider:

```toml
provider = "main"
model = "claude-sonnet-5-5"

[providers.main]
kind = "anthropic"
serve = ["claude-sonnet-5-5"]

[providers.reviewer]
kind = "openai"
serve = ["gpt-5"]          # models add-ons may ask through `llm`

[[plugins]]
name = "review"
path = "review.wasm"
config = { model = "gpt-5", kernel = "shell", dir = "{workspace}" }
grants = { fs-read = ["{workspace}", "{workspace}/*"], kernel = "shell", llm = "gpt-5" }
```

`corduroy check` lists `providers main, reviewer` and `tool review (read-only)`. Use any
model your key can reach: change it in `serve`, `config.model` and the `llm` grant together.

The workspace must be a git repository: `review` sends what `git diff HEAD` shows and the
files git does not track yet. Commit before a task, and the review covers exactly that task.

## Ask for the review

The tool's description tells the model to use it before it reports the work done. To be
sure, say so in the prompt or in `system`:

```bash
corduroy run "Add a --verbose flag to the CLI. When it works, call review with what the change is for, fix what it finds, and say what you did not fix and why."
```

```text
▸ review {"goal":"add a --verbose flag"}
  ✓ Review by gpt-5 of the changes against `HEAD`: …
```

`review` is read-only, so the `approval` gate's `smart` mode lets it run without asking.
`base` compares with something else (`main`, `HEAD~3`); `paths` adds files the reviewer
should read whole; `instructions` in the config tells every review what to look for:

```toml
config = { model = "gpt-5", kernel = "shell", dir = "{workspace}", instructions = "Check error handling and that new code has tests." }
```

## The other way round: GPT writes, Claude reviews

```toml
provider = "main"
model = "gpt-5"

[providers.main]
kind = "openai"
serve = ["gpt-5"]

[providers.reviewer]
kind = "anthropic"
serve = ["claude-sonnet-5-5"]

[[plugins]]
name = "review"
path = "review.wasm"
config = { model = "claude-sonnet-5-5", kernel = "shell", dir = "{workspace}" }
grants = { fs-read = ["{workspace}", "{workspace}/*"], kernel = "shell", llm = "claude-sonnet-5-5" }
```

## A reviewer with tools: two sessions

The `review` add-on's reviewer sees only what it is sent. For a reviewer that reads the code,
runs the tests and looks around, run it as a session of its own on the other provider. Each
session is a whole agent with every tool; they share the workspace and the log:

```bash
corduroy --session build run "Add a --verbose flag to the CLI."
```

```bash
corduroy --session review --provider reviewer --model gpt-5 run "Review the uncommitted changes (git diff HEAD): run the tests, read what the diff touches, and list what is wrong. Change nothing."
```

```bash
corduroy --session build run "A reviewer said: <paste>. Fix what is right, say why not for the rest."
```

Run them one after another: one `corduroy` at a time on a log. The model is chosen by the
command line, not kept with the session, so give `--provider` and `--model` each time you
come back to the reviewer's session.

## Limits

- A review must come back within two minutes (the `llm` bound for add-ons). A model that
  thinks long over a large diff can run out of time; review smaller changes, or lower
  `max_bytes`.
- The reviewer is asked once and has no tools. Within a run, a sub-agent (`delegate`)
  always runs on the main model. That a child session is started with a model, tools and
  limits of its own is decided and not built yet (see
  [What an extension can reach](../design.md#what-an-extension-can-reach)).
- What the reviewer is sent is recorded in the log as `llm/request`, and its answer as
  `llm/response`, so `corduroy log` shows every review.
