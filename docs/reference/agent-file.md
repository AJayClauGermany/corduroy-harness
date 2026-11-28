# The agent file

An agent file is a TOML file describing everything one agent can do: the model it asks, the
providers, kernels, secrets, MCP servers and extensions it has, and the add-ons it loads with
what each is granted. `corduroy` reads `corduroy.toml` in the workspace, or the file given
with `--agent`; with neither, it uses the default agent on a model served on this machine
by Ollama, which needs no key. `corduroy init` writes that agent out for the vendor you
name, with where its key comes from.

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
| `max_turns` | integer | 25 (the default agent sets 50) | Model turns a run may take before it ends as `turn-limit`. |
| `max_seconds` | integer | none: no limit | How long a run may go on before it ends as `time-limit`. Looked at before each request to the model, and a request still being answered when the time is up is dropped; a tool call under way is not cut short. Counted from when the command takes the run on, the sub-agents it runs included: time spent waiting for your answer is not counted, and `continue` gives a run its whole time again. |
| `max_output_tokens` | integer | none: no limit | How many tokens the model may write in a run, thinking included, before it ends as `output-limit`. Looked at before each request, so the request that passes it is answered whole and its tools are called. Counted from your last prompt, as the provider reports them, so `continue` goes on from what the run had written. Not the same as a provider's [`max_tokens`](#providersname), which is the most *one answer* may take. |

A run that reaches one of the three is not a failed run: what it did is done and recorded,
and the next prompt goes on in the same conversation. The command says which it was, and
[exits with a code of its own](cli.md#exit-codes) for each.
| `log` | string | `{data}/log.jsonl` | The event log. A relative path is in the workspace. |

## `[providers.<name>]`

A model provider, built from [oi-kit's providers](../../crates/corduroy-providers). The name
is what `provider` refers to.

| Key | Type | Meaning |
| --- | --- | --- |
| `kind` | string | `anthropic`, `openai`, `ollama`, `anthropic-compatible`, `openai-compatible`, or the name of another provider oi-kit knows (`corduroy_providers::known`). |
| `base_url` | string | Where the API is. Each kind has a default; the `-compatible` kinds need one. |
| `api_key_env` | string | The environment variable holding the key, read once when the agent starts. Defaults: `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`; Ollama needs none. |
| `secret` | string | The name of a [`[secrets.<name>]`](#secretsname) entry holding the key, in place of `api_key_env`. It is read for every request, so a value that changes while the agent runs is the one sent, and nothing is read from the environment unless that entry says so. For `anthropic`, `openai` and their `-compatible` kinds. |
| `headers` | table | Extra headers sent with every request. |
| `timeout_seconds` | integer | How long one attempt at a request may take in all, from sending it to the end of its answer, whether the answer is streamed or not. Default: 600. An attempt that runs out ends the request, and the run, as failed with the kind `timeout`; it is not tried again. A request tried again for another reason (a rate limit, a dropped connection) has the whole time for each attempt. |
| `streaming` | boolean | Whether the provider's API is asked to stream its response. Each kind has a default. Either way the answer is shown as it comes where the API gives it in pieces, checkpointed in the log (`model/progress`), and bounded as a whole by `timeout_seconds`; an API that is not asked to stream gives the answer as one piece. |
| `max_tokens` | integer | The most one answer may take. Default: what the model may write, as [`output_tokens`](#providersnamemodelsmodel) states it or as oi-kit's catalogue has it; for a model neither describes, a quarter of its stated `context_tokens` (at least 1024), or 4096. So a request always carries a limit, and a model served on this machine is not left to write until its context is full. A request to OpenAI's Responses API (the `openai` kinds asked for an o-series, GPT-5 or GPT-6 model, or given a `base_url` ending in `responses`) carries a limit only when this is set. |
| `temperature` | float | Sampling temperature. Default: the model's own. |
| `thinking` | string | `off`, `low`, `medium`, `high` or `max`, for a model that can think before it answers. |

An entry that is wrong (an unknown kind, a missing key) stops the agent from being
assembled, or leaves a spare provider out. A machine whose certificates cannot be loaded
does not: the provider is built without its HTTP client, so `corduroy check` and whatever
else asks no model still work, and the first request fails saying why, naming
`SSL_CERT_FILE` or `SSL_CERT_DIR` where one is set and nothing could be read from it. That
holds for a provider at `http://127.0.0.1` too, whose client is set up the same way.

A request that fails in a way worth trying again (a rate limit, a server error, a dropped
connection) is made again three times at most, each time recorded as
[`model/retry`](events.md#the-run).

A key the entry does not know is an error. Nothing here lists the models add-ons may ask:
an add-on names a model as `<name>/<model>` (this entry's name, then the provider's own name
for the model), and its [`llm` grant](capabilities.md) says which it may ask.

### `[providers.<name>.models.<model>]`

What the file states or corrects about one model of the provider, by the provider's name for
it. A model describes itself as far as its provider knows (oi-kit's catalogue, for the hosted
ones); a model served on this machine usually reports nothing, so this is where to say what
it holds. Every key is optional, and what is left out stays as the provider reports it.

| Key | Type | Meaning |
| --- | --- | --- |
| `context_tokens` | integer | How many tokens the model holds at once. [`compact`](shipped-add-ons.md#compact) starts from three quarters of it, an answer may take a quarter of it where nothing else says how much the model may write (see `max_tokens`), and the harness holds one tool result to an eighth of it and the results of one step to a quarter ([Design](../design.md#keeping-within-what-the-model-holds)). Without it nothing bounds a step's results but each tool's own cap. |
| `output_tokens` | integer | The most it may write in one answer: the default for the provider's `max_tokens`. |
| `takes` | list of strings | The kinds of content it takes: `text`, `image`, `audio`, `video`, `pdf`, or another kind by name. A list given here replaces the provider's whole. |
| `returns` | list of strings | The kinds of content it returns, named the same way. |
| `tools` | boolean | Whether it can be offered tools. |

## `[kernels.<name>]`

A kernel: a long-lived process the agent runs code in, keeping its state between calls. The
name is what a `kernel` grant and a plugin's configuration refer to.

| Key | Type | Meaning |
| --- | --- | --- |
| `kind` | string | `shell` (a persistent POSIX shell) or `powershell`, or a kernel kind a command of your own adds. `<kind>` builds the extension kind `<kind>-kernel`. |
| `dir` | string | Where it starts. Default `{workspace}`. |
| `program` | string | The shell to run. `shell`: `sh` by default, any POSIX shell such as `bash`. `powershell`: `powershell` on Windows, `pwsh` elsewhere. |
| `env` | table | Environment variables to set for the shell and for what the kernel starts, such as `env = { CARGO_TERM_COLOR = "never" }`. |
| `pass` | list of strings | Variables of your own environment to hand through by name, such as `pass = ["CARGO_HOME", "HTTPS_PROXY"]`. One that is not set is left out. |

**The environment.** A kernel's shell, and an MCP server it starts, begin from an empty
environment and not from the one `corduroy` runs in, which is where a provider's API key
is. They are given the plain variables a process needs to run (`PATH`, `HOME`, `USER`,
`LANG`, `TZ`, `TMPDIR`, `TERM` and, on Windows, the system's own such as `SystemRoot`), each
as you have it, and beyond those only what `env` sets and `pass` names. Whatever is given
this way is in the open: a command can print it, and what it prints is in the log. So
`pass` a credential only when a command needs the value itself.

A kernel is not a sandbox: a shell kernel can do whatever the user running `corduroy` can.
Which kernel an add-on may use is the security decision. See
[Security model](../explanation/security-model.md).

## `[secrets.<name>]`

A secret: what a provider sends as its key when its entry names it (`secret`), and what an
add-on may use through the `secret` capability without ever seeing its value.

| Key | Type | Meaning |
| --- | --- | --- |
| `kind` | string | Where the value comes from: `keychain`, `env` or `chatgpt`, unless the command brings other kinds. Left out, it is `keychain`, unless the entry has an `env` key: then it is `env`. The entry's other keys are that kind's. |
| `env` | string | For `env`: the environment variable holding the value, read when a request needs it. |
| `hosts` | list of strings | For `env`, and a [prototype](../prototypes.md#x1-a-secret-names-the-hosts-it-may-go-to): the hosts the value may be sent to, each a host name or a pattern over them as an [`http` grant's](capabilities.md) are (`["api.github.com"]`, `["*.example.org"]`). A provider or an MCP server that names the secret, or an add-on's request that carries it, is refused it for any other host. An empty list names none, and the secret goes nowhere. Left out, the secret is sent as before, wherever what uses it sends it. |
| `keep` | string | For `chatgpt`: where the sign-in is kept, `keychain` (the default) or `file`. |

The keychain is where a secret is kept unless the entry says otherwise, so the safer source
is the one that needs nothing written: `[secrets.github]` and no more is a secret kept in
the keychain. Reading the environment is said by naming the variable (`env = "GITHUB_AUTH"`),
and keeping a sign-in in a file by `keep = "file"`. A machine without a keychain is not
given either by itself: the entry fails, saying so, until it says one of them.

A `keychain` secret takes no other key. Its value is kept in the operating system's
keychain (the macOS Keychain, the Windows Credential Manager, the Secret Service on Linux),
encrypted behind your own login, under the service `corduroy` and the entry's name (letters,
digits, `-` and `_`). You put it there with [`corduroy secrets set <name>`](cli.md), which
does not show it; it is never in the agent file, the environment or the log. It is read when
first needed and then kept for the rest of that run of the command, so the keychain asks you
at most once. The keychain is yours and not the workspace's: every agent of yours that names
a secret alike reads the same item. There is no fallback: on a machine without a keychain
(a server nobody is logged in to, a container) revealing it fails and says so, and the
entry has to name another source. [`corduroy secrets`](cli.md) says how each secret stands.

The hosts a `keychain` secret may be sent to are not said in the agent file but beside the
value, by whoever puts it in (`corduroy secrets set <name> --hosts api.anthropic.com`), so
that an agent file somebody else wrote cannot name your key and send it elsewhere. This is
a [prototype](../prototypes.md#x1-a-secret-names-the-hosts-it-may-go-to): the hosts are
kept as a second item, `<name>.hosts`, and a value put in without any is sent as before.

```toml
[providers.main]
kind = "anthropic"
secret = "anthropic"

[secrets.anthropic]
kind = "keychain"         # corduroy secrets set anthropic
```

A `chatgpt` secret takes `keep` and no other key. Its value is the access token of a sign-in
with ChatGPT, made with [`corduroy login`](cli.md) and kept under the entry's name (letters,
digits, `-` and `_`): in the keychain, as the item `chatgpt-sign-in.<name>`, or with
`keep = "file"` in your own configuration directory, readable by you alone. A sign-in made
while a file was where one went is still in its file: `corduroy secrets` says so, and the
next `corduroy login` moves it into the keychain without the browser.
The token is renewed as it runs
out, so the secret always reveals one that is good; if nobody is signed in, or the sign-in
was revoked, revealing it fails and says to run `corduroy login`. Name it as the `secret`
of an `openai` provider to have that provider's requests paid by your ChatGPT plan.

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
| `secret` | string | The name of a [`[secrets.<name>]`](#secretsname) entry sent as the bearer token for `url`: a token kept in the keychain, or wherever that entry says. It is read once, when the server is connected to. |
| `bearer_token_env` | string | The environment variable holding a bearer token for `url`, where no `secret` is named. Give one or the other. |
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
| `page` | `port` (any free one by default) | the agent's [page in a browser](../how-to/watch-a-run-in-a-browser.md), served on `127.0.0.1` while `corduroy` runs the agent: what it does as it does it, a gate's question answered there, a prompt typed and a run stopped. The command says its address when it starts. Not in the default agent until you take the `#` off its two lines |

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
| `replaces` | list of strings | none | The tools it offers in the place of tools an add-on listed before it registered, by name. Without it, the second add-on to offer a name does not start. With it, each tool named takes the name over, and only if it can be taken for the tool it replaces: the same input schema (as JSON, spacing aside) and the same word on whether a call may be made again after a crash. A tool that differs, a name nothing has registered, or a name this add-on does not offer stops the add-on from starting, and says which. What it replaced is called again once this add-on is unloaded. See [Replace a shipped tool](../how-to/replace-a-shipped-tool.md). |
| `enabled` | boolean | `true` | `false` keeps the entry without loading it. |

**The plugin path.** A relative `path` is looked for, in order, in each directory in
`CORDUROY_PLUGIN_PATH`, the `plugins` directory beside the `corduroy` program (symbolic links
followed; where a [ready-made build](../how-to/install-a-build.md) keeps its add-ons), the
`plugins/target` directories of the source tree the command was built from (where
`cargo plugins` and `scripts/guests.sh` put the add-ons), and last the agent file's
directory. That is why the default agent can say `path = "pi_tools.wasm"`. The add-ons that
ship with the command come before anything beside the agent file, so a repository's own
`approval.wasm` never stands in for the shipped one; give an add-on of your own another file
name, or a path from `{workspace}`.

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
