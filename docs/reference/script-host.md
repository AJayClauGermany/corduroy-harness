# The script host

One add-on, `script_host_js.wasm` ([`sdk/js/script-host`](../../sdk/js/script-host/host.js),
built by `scripts/guests.sh`), that runs automations written as JavaScript text. Nothing is
compiled for an automation: the same component is loaded once for each, with that
automation's script as its configuration and its own grants.

A script depends on three things, and this page is the list of them: the configuration the
component takes, the `host` object the script is handed, and the plugin object it returns.
Their names and shapes are meant to hold for all of 1.x, as the world they mirror does
([How the plugin world grows](../design.md#how-the-plugin-world-grows)). `host` is not a
capability and grants nothing: each call through it is checked against the automation's own
grants, like any add-on's.

## Configuration

| Key | What it is |
| --- | --- |
| `script` | Required. The body of a function, as text. It is called once at activation with `host`, `ToolError`, `CallError` and `Content` in reach, and returns the plugin object. |
| `name` | The name the automation's tools, events and state go under (`script` when absent). |
| `config` | Handed to the plugin object's `activate`, if it has one (`{}` when absent). |

An instance that trapped is activated again with the same configuration, so the script runs
again from the top: keep what must last in `host.state`.

## Grants

The component imports every capability, and an add-on is started only with every
capability it imports. So an automation's entry names all sixteen, with an empty list for
each one it may not use: that links the capability and lets nothing through. A call outside
a grant is refused as `denied`.

```toml
[[plugins]]
name = "stamp"
path = "script_host_js.wasm"
config = { name = "stamp", script = """
return {
  tools: {
    stamp: {
      description: 'Write the time to a file.',
      input: { type: 'object', properties: { path: { type: 'string' } }, required: ['path'] },
      run({ path }) {
        host.fsWrite.write(path, new Date().toISOString());
        return 'stamped';
      },
    },
  },
};
""" }
[plugins.grants]
fs-write = ["{workspace}/*"]
clock = "wall"
fs-read = []
kernel = []
events = []
log = []
random = []
tools = []
state = []
service = []
plugins = []
llm = []
secret = []
http = []
log-read = []
session = []
```

Because it imports `tools` and `plugins`, an automation cannot be a gate at `tool/call` or
`plugin/load`: the host refuses to start one that registers there.

## What the script is handed

| Name | What it is |
| --- | --- |
| `host` | The capabilities, by name (below). Frozen. |
| `ToolError` | `new ToolError(kind, message)`, with `kind` one of `invalid-input`, `failed`, `denied`, `unknown-tool`: thrown from a tool to choose its error. |
| `CallError` | `new CallError(kind, message)`, with `kind` one of `unknown`, `invalid-input`, `failed`, `denied`: thrown from a service's method. |
| `Content` | `Content.text(value)`, `Content.image(mimeType, bytes)` and `Content.textOf(blocks)`: the blocks of a tool's result. |

Any other exception a tool throws becomes `failed`; a host call's `denied` keeps its kind.
`console.*`, `Date`, `performance.now()`, `Math.random()` and `crypto.getRandomValues()` go
to `log`, `clock` and `random` and throw without the grant, saying what to grant (`console`
is dropped instead); `fetch` always throws.

## `host`

Each member is one capability of [the plugin world](capabilities.md), with its functions
under their names in camel case. A function the world gives a `result` returns the value
and throws the error. Records are objects with camel-case keys; a list of bytes is a
`Uint8Array`; a 64-bit number is a `BigInt`; an option is the value or `undefined`.

| Member | Capability | Functions |
| --- | --- | --- |
| `host.fsRead` | `fs-read` | `read(path)`: the file's text. `list(path)`: names in a directory, directories ending in `/`. |
| `host.fsWrite` | `fs-write` | `write(path, contents)`, `remove(path)`. |
| `host.kernel` | `kernel` | `exec(kernel, code, timeoutMs)`: `{ output, status, truncated }`. |
| `host.events` | `events` | `emit(topic, payload)`: queued, delivered when the call returns. |
| `host.log` | `log` | `write(level, message)`, with `level` one of `debug`, `info`, `warn`, `error`. |
| `host.clock` | `clock` | `now()`: `{ seconds, nanoseconds }`. `monotonic()`: nanoseconds. |
| `host.random` | `random` | `getBytes(len)`. |
| `host.tools` | `tools` | `list()`: the tools the grant reaches. `call(name, input)`: the result's blocks, `input` as JSON text. |
| `host.state` | `state` | `get(key)`, `set(key, value)`, `remove(key)`, `keys()`. |
| `host.service` | `service` | `call(service, method, payload)`: the reply's text. |
| `host.plugins` | `plugins` | `load({ name, path, config, grants })`: the component's SHA-256. `unload(name)`, `loaded()`. |
| `host.llm` | `llm` | `complete(model, { system, messages })`, each message `{ role, text }` with `role` `user` or `assistant`: the answer's text. |
| `host.secret` | `secret` | `get(name)`: a credential, whose value stays in the host. |
| `host.http` | `http` | `send({ method, url, headers, body })`: `{ status, headers, body }`. A header's value is `{ tag: 'text', val }` or `{ tag: 'secret', val: credential }`. |
| `host.logRead` | `log-read` | `read(session, topic, after, limit)`: events, oldest first. |
| `host.session` | `session` | `current()`, `call()`, `stopping()`, `resultBudget()`. |

What each function does, its limits and how its grant is scoped are in
[Capabilities](capabilities.md) and in [`wit/corduroy.wit`](../../wit/corduroy.wit), which
is the contract.

## The plugin object

What the script returns. Every field is optional.

| Field | Becomes |
| --- | --- |
| `activate(config)` | Called once the script has returned, with the configuration's `config`. |
| `tools: { name: { description, input, readOnly, replaySafe, result, run(input) } }` | Tools for the model. `input` is a JSON Schema (any object when absent); `result: { tokens, keep }` says the most one result should take and which part of a longer one is kept (`head`, `tail` or `ends`); `run` returns text, anything else as JSON, or `Content` blocks. |
| `subscriptions`, `onEvent(event)` | Events whose topic matches one of the globs. An event is `{ id, session, source, cause, topic, payload }`, `payload` as JSON text. |
| `prompt()` | Text for the system prompt of the next request, or `undefined`. |
| `gates: { point: (request) => 'allow' \| { ask } \| { deny } }` | A gate at a decision point other than `tool/call` and `plugin/load`. One that throws denies. |
| `services: { name: { method(payload, call) } }` | Services other plugins call; `call` says who is calling. |

A `name` the object carries is ignored: an automation's name is its configuration's. The
fields are the same as those of a plugin built with the
[JavaScript SDK](../how-to/write-add-ons-in-js-and-python.md), which is what to use when an
add-on should import only the capabilities it needs.
