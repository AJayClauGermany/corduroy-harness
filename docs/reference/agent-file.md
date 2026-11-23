# The agent file

An agent file is a TOML file describing everything one agent can do: the model it asks, the
providers, kernels, secrets, MCP servers and extensions it has, and the add-ons it loads with
what each is granted. `corduroy` reads `corduroy.toml` in the workspace, or the file given
with `--agent`; with neither, it uses the default agent, which `corduroy init` writes out.

Unknown keys are errors, so a misspelt grant is never silently ignored. Keys are only ever
added to this format, never renamed (see [What is stable](../design.md#what-is-stable)).

Source: [`crates/corduroy-cli/src/agent.rs`](../../crates/corduroy-cli/src/agent.rs).

## Placeholders

Every string in the file (paths, grants, configuration) may use:

| Placeholder | Becomes |
| --- | --- |
| `{workspace}` | the workspace: `--workspace`, or the current directory |
| `{data}` | `{workspace}/.corduroy`, where the log, skills and memories are kept |
| `{home}` | `$HOME` (`%USERPROFILE%` on Windows) |

## Top-level keys

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `provider` | string | required | The provider the loop asks: the name of a `[providers.<name>]` entry. `--provider` overrides it. |
| `model` | string | required | The model to ask that provider for. `--model` overrides it. |
| `system` | string | none | The start of every system prompt. Plugins' prompt parts follow it. |
| `max_turns` | integer | 25 (the default agent sets 50) | Model turns a run may take before it ends as `turn limit reached`. |
| `log` | string | `{data}/log.jsonl` | The event log. A relative path is in the workspace. |

## `[providers.<name>]`

A model provider, built from [oi-kit's providers](../../crates/corduroy-providers). The name
is what `provider` refers to.

| Key | Type | Meaning |
| --- | --- | --- |
| `kind` | string | `anthropic`, `openai`, `ollama`, `anthropic-compatible`, `openai-compatible`, or the name of another provider oi-kit knows (`corduroy_providers::known`). |
| `base_url` | string | Where the API is. Each kind has a default; the `-compatible` kinds need one. |
| `api_key_env` | string | The environment variable holding the key. Defaults: `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`; Ollama needs none. |
| `headers` | table | Extra headers sent with every request. |
| `timeout_seconds` | integer | How long one request may take in all. |
| `streaming` | boolean | Whether to stream responses. Each kind has a default. |
| `max_tokens` | integer | The most one answer may take. Default: the model's own limit from oi-kit's catalogue, or 4096 for a model it does not know. |
| `temperature` | float | Sampling temperature. Default: the model's own. |
| `thinking` | string | `off`, `low`, `medium`, `high` or `max`, for a model that can think before it answers. |
| `serve` | list of strings | Models of this provider that add-ons may ask through the `llm` capability. |

## `[kernels.<name>]`

A kernel: a long-lived process the agent runs code in, keeping its state between calls. The
name is what a `kernel` grant and a plugin's configuration refer to.

| Key | Type | Meaning |
| --- | --- | --- |
| `kind` | string | `shell` (a persistent POSIX shell) or `powershell`, or a kernel kind a command of your own adds. `<kind>` builds the extension kind `<kind>-kernel`. |
| `dir` | string | Where it starts. Default `{workspace}`. |
| `program` | string | The shell to run. `shell`: `sh` by default, any POSIX shell such as `bash`. `powershell`: `powershell` on Windows, `pwsh` elsewhere. |

A kernel is not a sandbox: a shell kernel can do whatever the user running `corduroy` can.
Which kernel an add-on may use is the security decision. See
[Security model](../explanation/security-model.md).

## `[secrets.<name>]`

A secret an add-on may use through the `secret` capability without ever seeing its value.

| Key | Type | Meaning |
| --- | --- | --- |
| `env` | string | The environment variable holding the value, read when a request needs it. |

## `[mcp.<name>]`

An MCP server. Its tools are offered to the model as `<name>__<tool>`. Give either `command`
or `url`, not both.

| Key | Type | Meaning |
| --- | --- | --- |
| `command` | string | A program speaking MCP over its standard input and output. It is started through a kernel, never directly. |
| `args` | list of strings | Its arguments. |
| `env` | table | Environment variables to set for it. |
| `kernel` | string | The kernel that starts it. Default `shell`; it must have a `[kernels.<name>]` entry. |
| `url` | string | A server reached over streamable HTTP. |
| `bearer_token_env` | string | The environment variable holding a bearer token for `url`. |
| `timeout_seconds` | integer | How long to wait for it to start, and for each tool call. |

## `[extensions.<name>]`

An extension: a native plugin of a kind the command was built with. `kind` names the kind;
every other key is the extension's own configuration, and a kind refuses keys it does not
know.

| Kind (shipped) | Configuration | Adds |
| --- | --- | --- |
| `shell-kernel` | `dir`, `program` | a shell kernel named like the entry (what `[kernels.*]` with `kind = "shell"` builds) |
| `powershell-kernel` | `dir`, `program` | a PowerShell kernel named like the entry |
| `mcp` | as `[mcp.*]` | one MCP server's tools |
| `env-secret` | `env` | one secret |
| `http-client` | none | the client the `http` capability sends through; every agent gets it already |

A command of your own can add kinds; see [Add an extension kind](../how-to/add-an-extension-kind.md).

## `[[plugins]]`

An add-on: a WebAssembly component with its grants. Add-ons are loaded in the order listed,
after every native plugin above.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | required | The name it is loaded under. It owns its events, its state and its place as a caller under this name; no two plugins can share one. |
| `path` | string | required | The `.wasm` file: absolute, or found on the plugin path (below). |
| `config` | any TOML value | none | Its configuration, handed to its `activate` as JSON. |
| `grants` | table | none | Each capability it is granted, with its scope (below). |
| `enabled` | boolean | `true` | `false` keeps the entry without loading it. |

**The plugin path.** A relative `path` is looked for, in order, in the agent file's directory,
each directory in `CORDUROY_PLUGIN_PATH`, and the `plugins/target` directories of the source
tree the command was built from (where `cargo plugins` and `scripts/guests.sh` put the
add-ons). That is why the default agent can say `path = "pi_tools.wasm"`.

### Grants

`grants` maps a [capability](capabilities.md) to a scope, written in one of three ways:

```toml
[plugins.grants]
kernel = "shell"                                   # one pattern
fs-read = ["{workspace}", "{workspace}/*"]         # any of these patterns
fs-write = { allow = ["{workspace}/*"], deny = ["{workspace}/.git/*"] }   # deny wins
```

In a pattern, `*` matches any run of characters, `/` included. A grant of `"*"` reaches
everything the capability covers; an empty list (`kernel = []`) links the capability, so the
plugin starts, but lets nothing through. What a pattern is matched against depends on the
capability: an absolute path, a kernel name, a topic and so on (see
[Capabilities](capabilities.md)).

A plugin is linked only to the capabilities its component imports. If it imports one that
is not granted here, it does not start, and `corduroy check` says which. A grant for a
capability it does not import is harmless. A capability name that does not exist is an
error.

Whatever a grant says, the agent file, the log and the compiled-code cache cannot be read or
written through `fs-read` and `fs-write`.

## A complete example

```toml
system = "You are a careful coding agent working in {workspace}."
provider = "main"
model = "claude-sonnet-5-5"
max_turns = 50

[providers.main]
kind = "anthropic"
serve = ["claude-sonnet-5-5"]
thinking = "medium"

[kernels.shell]
kind = "shell"

[secrets.github]
env = "GITHUB_TOKEN"

[mcp.fetch]
command = "uvx"
args = ["mcp-server-fetch"]

[[plugins]]
name = "pi-tools"
path = "pi_tools.wasm"
config = { kernel = "shell" }
[plugins.grants]
fs-read = ["{workspace}", "{workspace}/*"]
fs-write = { allow = ["{workspace}", "{workspace}/*"], deny = ["{workspace}/.git/*"] }
kernel = "shell"

[[plugins]]
name = "approval"
path = "approval.wasm"
config = { mode = "smart", allow = ["todo_write"] }
```

`corduroy init` writes the full default agent, with a comment on each entry; it is the best
starting point. [`examples/local-model/corduroy.toml`](../../examples/local-model/corduroy.toml)
is the same agent with a model on this machine.
