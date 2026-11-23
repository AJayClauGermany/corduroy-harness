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
capability. A provider offers a model to them only if it lists it under `serve`, and each
add-on's `llm` grant must reach it:

```toml
[providers.main]
kind = "anthropic"
serve = ["claude-haiku-5-5"]

[[plugins]]
name = "summarize"
path = "summarize.wasm"
config = { model = "claude-haiku-5-5" }
grants = { fs-read = ["{workspace}", "{workspace}/*"], llm = "claude-haiku-5-5" }
```

Using a smaller model here than the main one saves the main model's context and cost.

## Check it

```bash
corduroy check
```

The first line names the model and provider. `check` does not call the model; a wrong key
shows up on the first `run`, as `the run failed: …`.
