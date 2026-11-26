# Connect a model

The loop asks the provider named by `provider` for the model named by `model`. Both are set
in the [agent file](../reference/agent-file.md); every provider is a `[providers.<name>]`
entry.

## Anthropic or OpenAI

```toml
provider = "main"
model = "claude-sonnet-5-5"

[providers.main]
kind = "anthropic"        # reads ANTHROPIC_API_KEY; "openai" reads OPENAI_API_KEY
```

To read the key from another variable, add `api_key_env = "MY_KEY"`.

## Name a secret instead

A provider can take its key from a `[secrets.<name>]` entry. The value is then read for
every request, not once at the start, so a key that is replaced while the agent runs is the
one sent:

```toml
[providers.main]
kind = "openai"
secret = "openai-key"

[secrets.openai-key]
env = "MY_OPENAI_KEY"
```

## Ollama

```toml
provider = "main"
model = "qwen3"

[providers.main]
kind = "ollama"           # http://localhost:11434 unless base_url says otherwise
```

## Any OpenAI- or Anthropic-compatible endpoint

```toml
[providers.main]
kind = "openai-compatible"                    # or "anthropic-compatible"
base_url = "https://llm.example.com/v1/chat/completions"
api_key_env = "EXAMPLE_API_KEY"
headers = { "x-team" = "research" }           # sent with every request
```

Other providers oi-kit knows can be named directly as the `kind`.

## A small model on this machine, with no key

```bash
scripts/local-model.sh start
```

This builds llama.cpp's server and downloads LFM3 1.7B (both pinned by SHA-256, about four
minutes the first time) and serves it on `127.0.0.1:8080`. Then:

```toml
[providers.main]
kind = "openai-compatible"
base_url = "http://127.0.0.1:8080/v1/chat/completions"
timeout_seconds = 300
```

The server answers to any model name. `scripts/local-model.sh stop` stops it.

A model served on this machine reports nothing about itself. State what it holds, under the
name you gave as `model`, so that `compact` condenses a long conversation before the context
fills (it starts at three quarters of it):

```toml
[providers.main.models.local]
context_tokens = 16384
```

[`examples/local-model/corduroy.toml`](../../examples/local-model/corduroy.toml) is a whole
agent set up this way.

## Tune the answers

```toml
[providers.main]
kind = "anthropic"
max_tokens = 16000        # the most one answer may take
temperature = 0.2
thinking = "medium"       # off, low, medium, high or max, for a model that can think
timeout_seconds = 120
streaming = true
```

## Keep several and switch

Define as many providers as you like and pick one per run without editing the file:

```toml
provider = "cloud"
model = "claude-sonnet-5-5"

[providers.cloud]
kind = "anthropic"

[providers.local]
kind = "ollama"
```

```bash
corduroy --provider local --model qwen3 run "summarise README.md"
```

## Let add-ons ask a model

Add-ons such as `summarize` and `compact` ask a model of their own through the `llm`
capability. An add-on names a model as `<provider>/<model>`: the provider's name in the agent
file, then the provider's own name for the model. Its `llm` grant is the one list of the
models it may ask, written the same way:

```toml
[providers.main]
kind = "anthropic"

[[plugins]]
name = "summarize"
path = "summarize.wasm"
config = { model = "main/claude-haiku-5-5" }
grants = { fs-read = ["{workspace}", "{workspace}/*"], llm = "main/claude-haiku-5-5" }
```

A grant is a pattern, so `llm = "main/*"` is every model of that provider, and
`llm = { allow = "main/*", deny = "main/claude-opus-*" }` all of them but some.

Using a smaller model here than the main one saves the main model's context and cost.

The same way, the `review` add-on can have a model from another provider check the main
model's work: see [Have a second model review the work](review-with-a-second-model.md).

## Check it

```bash
corduroy check
```

The first line names the model and provider. `check` does not call the model; a wrong key
shows up on the first `run`, as `the run failed: …`.
