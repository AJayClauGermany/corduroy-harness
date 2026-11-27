# Decide what runs without asking

Two shipped gates guard every tool call in the default agent: `approval` (permission modes)
and `inspect` (dangerous commands, credentials, repetition). When several gates answer, the
strictest wins: deny over ask over allow.

## Pick a mode

```toml
[[plugins]]
name = "approval"
path = "approval.wasm"
config = { mode = "smart" }
```

| `mode` | Read-only tools | Tools that change things |
| --- | --- | --- |
| `auto` | run | run |
| `smart` (default) | run | ask you |
| `approve` | ask you | ask you |
| `chat` | refused | refused: the agent only talks |

What is read-only is each tool's own word (`corduroy check` marks them). In the default
agent `read`, `tree`, `grep` and `find` are, so the agent looks around without asking;
`bash` is not, whatever the command, and neither is `web_fetch`, which changes nothing here
but sends an address out.

## Always allow, or never allow, particular tools

```toml
config = { mode = "smart", allow = ["todo_write", "memory_save"], deny = ["bash"] }
```

Names are exact tool names (`corduroy check` lists them; MCP tools are `<server>__<tool>`).
`deny` wins over `allow`, and both win over the mode. To have pages fetched without a
question each time, allow `web_fetch`, and consider keeping its `http` grant to the hosts
you mean ([fetch](../reference/shipped-add-ons.md#fetch)).

## Run unattended

For a run nobody will answer (CI, a script, a disposable sandbox) use `auto`:

```toml
config = { mode = "auto" }
```

`auto` lets `bash` run anything in the shell kernel, which is not a sandbox. Keep the
add-ons' `fs-*` grants narrow, and run in a workspace you can throw away. `inspect` still
asks about destructive commands; with nobody to answer, the run waits (below).

## Ask about more, or refuse more

`inspect` takes extra substrings to look for in a call's input:

```toml
[[plugins]]
name = "inspect"
path = "inspect.wasm"
config = { ask = ["kubectl delete", "DROP TABLE"], deny = ["terraform destroy"], max_repeats = 3 }
```

For anything more particular, [write a gate](write-a-gate.md).

## Answer later

A question is put at the terminal and, where the agent has a `page` extension turned on, on
[its page](watch-a-run-in-a-browser.md) with **Allow** and **Refuse**. The
first answer given is the answer.

At the `allow? [y/N, or a reason to refuse]` prompt:

| Type | And |
| --- | --- |
| `y` | the call runs |
| Enter or `n` | it is refused; the model is told you declined |
| any other text | it is refused, and your text is the reason the model is given |
| Ctrl-D | nothing is decided; the run waits, in the log |

A waiting run survives the process. Answer whenever you like:

```bash
corduroy continue
```

A run that waits with no terminal (`corduroy run … < /dev/null`) exits with status 1 and
`waiting for 1 answer(s)`. To abandon it instead:

```bash
corduroy stop
```

## Turn a gate off

Set `enabled = false` on its entry, or remove the entry. With no gate at all, every call
runs and nothing is asked or recorded about it.
