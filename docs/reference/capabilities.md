# Capabilities and the plugin world

An add-on is a WebAssembly component built against the world `corduroy:plugin/plugin` in
[`wit/corduroy.wit`](../../wit/corduroy.wit). The world has sixteen **imports**, each a
capability the host may grant, and six **exports**, the functions the add-on provides. There
is no WASI: an add-on has no clock, filesystem, network, environment or randomness except
through these imports.

## How a grant is applied

1. **At load.** Only the interfaces the add-on was granted are linked. An add-on that imports
   one it was not granted does not start (`plugin/failed`, and `corduroy check` names it).
2. **At each call.** The host resolves the resource (paths are made absolute and symbolic
   links followed) and checks it against the scope. A refusal comes back to the add-on as
   `denied` and is recorded as `capability/denied`. A refused path says where paths are
   relative to (the add-on's working directory) and nothing of what the grant covers: a
   model that made up an absolute path is told where to start from. Calls that list (`tools.list`,
   `state.keys`, `log-read.read`) leave out what the scope does not reach rather than refuse.

A scope is a set of allow patterns and deny patterns; deny wins. `*` matches any run of
characters, `/` included. A scope with no allow patterns links the interface but lets nothing
through. See [Grants](agent-file.md#grants) for how to write one in the agent file.

## The imports

| Capability | Lets an add-on | Scope matches | Example grant |
| --- | --- | --- | --- |
| `fs-read` | read text files (`read`), list directories (`list`) | absolute path | `["{workspace}", "{workspace}/*"]` |
| `fs-write` | create or replace (`write`, making parent directories) and delete (`remove`) files | absolute path | `{ allow = ["{workspace}/*"], deny = ["{workspace}/.git/*"] }` |
| `kernel` | run code in a long-lived kernel (`exec`, with a timeout); gets output, status and whether output was cut | kernel name | `"shell"` |
| `events` | publish an event (`emit`); `state/*` topics are refused | topic | `"skills/*"` |
| `log` | write a diagnostic line, recorded as `plugin/log` (16 KiB each) | level: `debug`, `info`, `warn`, `error` | `"*"` |
| `clock` | wall-clock time (`now`), and a monotonic clock starting with the instance | `wall`, `monotonic` | `"*"` |
| `random` | bytes from the host's secure generator (64 KiB per call) | `secure` | `"secure"` |
| `tools` | list and call other plugins' tools, through the gates | tool name | `["read", "tree"]` |
| `state` | a durable key-value store of its own (`get`, `set`, `remove`, `keys`), kept on the log | key | `"*"` |
| `service` | call a service another plugin provides | `<service>:<method>`; a service's name alone is every method of it | `"sessions"`, `"sessions:start"` |
| `plugins` | load and unload add-ons as its children, granting no more than it holds | the loaded plugin's name | `"*"` |
| `llm` | one completion from a model of a [provider](agent-file.md#providersname), recorded as `llm/request` and `llm/response` with what it cost | model, as `<provider>/<model>` | `"main/claude-haiku-5-5"`, `"main/*"` |
| `secret` | an opaque handle to a named credential, never its value | secret name | `"github"` |
| `http` | an HTTP request through the host's client, which writes credentials in and follows no redirects | host name | `"api.github.com"` |
| `log-read` | read events of any session, by topic, in pages of up to 1000 | `<session>:<topic>` (session percent-encodes `%` and `:`; empty for harness events) | `["*:message", "*:session/start"]` |
| `session` | which session and tool call it is serving, whether the run was asked to stop, and how much the call's result may take | session id | `"*"` |

Whatever a grant says:

- **Protected paths** (the host binary, the compiled-code cache, and, in the `corduroy`
  command, the agent file and the log) are refused to `fs-read` and `fs-write`.
- **Cascades end.** An add-on is not called for an event more than 16 causes deep.
- **Each call is metered:** a fuel budget per call, and capped memory per instance.
- **Outbound waits are bounded:** `llm` and `http` give up after a limit the host sets.

## The exports

Every add-on exports all six; the SDKs supply a default for each but `activate`.

| Export | Called | Returns |
| --- | --- | --- |
| `activate(config)` | when the add-on starts, and again on a fresh instance after a trap | `plugin-info`: its name and version, tools, event subscriptions, whether it has prompt text, the decision points it gates, the services it provides; or an error, and it does not start |
| `call-tool(name, input)` | for each call of one of its tools; `input` is JSON | a list of content blocks (text, images), or a `tool-error`: `unknown-tool`, `invalid-input`, `failed`, `denied`; either way the model sees it |
| `on-event(event)` | for each event whose topic matches one of its subscriptions, after the event is stored | nothing: events never return values |
| `prompt-part()` | before each model request, if it said it has prompt text | text for the system prompt, or none |
| `gate(point, request)` | at a decision point it gates; `request` is JSON | `allow`, `ask(reason)` or `deny(reason)`. The strictest answer of all gates wins. A gate that traps denies |
| `handle-call(call)` | for a call to a service it provides, with the caller's name | JSON text, or a `call-error`: `unknown`, `invalid-input`, `failed`, `denied` |

Two rules are checked from the component's imports at activation: an add-on that gates
`tool/call` may not import `tools`, and one that gates `plugin/load` may not import
`plugins` (each would be asked about its own calls).

## Tool specs

Each tool an add-on lists in `plugin-info` has:

| Field | Meaning |
| --- | --- |
| `name`, `description` | what the model sees |
| `input-schema` | JSON Schema of the input object, as JSON text |
| `read-only` | it only looks and changes nothing. Gates such as `approval` in `smart` mode let these through without asking |
| `replay-safe` | calling it twice is the same as once, so a call that was running when the process died may simply be made again |

## Decision points

A gate is asked only at a declared point. The core declares two, the loop one, and an
extension may declare more (`corduroy check` lists them under `points`).

| Point | Asked | Request |
| --- | --- | --- |
| `tool/call` | before each tool call, the model's or a plugin's | `{"session", "id", "tool", "input", "read-only", "caller"}`; `caller` is `agent` or `plugin:<name>` |
| `plugin/load` | before an add-on loads another | the loader, the child's name, the SHA-256 of its component, the capabilities it imports and the grants requested |
| `session/start` | before the `sessions` service starts a child session | `{"session", "parent", "starter", "task"}`: the id the session would have, the session whose tool call starts it, who asks (`plugin:<name>`) and what it is for. A [prototype](../prototypes.md#x5-the-decision-point-before-a-session-starts): the request gains what a session is started with as that is built |

An ask from a plugin's own tool call, at `plugin/load` or at `session/start` is a denial:
nobody is there to answer it. A host that embeds the harness can take a person's answers
at the last two ([a prototype](../prototypes.md#x9-an-ask-that-reaches-a-person-anywhere));
the `corduroy` command does not yet.

## Versioning

The package is `corduroy:plugin@0.1.0`. Before 1.0 a host loads only add-ons built for its
own version of the world. It reads the version from what an add-on imports and refuses one
built for another when it is loaded, naming both:

```text
not a valid plugin: it was built for the plugin world corduroy:plugin@0.2.0 and this host's
is corduroy:plugin@0.1.0: before 1.0 a host loads only add-ons built for its own version.
Build it again, against this host's world
```

A patch version (`0.1.1`) changes wording and no shape, so it loads.

From 1.0, an add-on that is already built loads on every 1.x host. A minor version adds a
capability, or a function to one where its grant already covers what the function does;
neither breaks an existing add-on. Changing an export, a shared type or a function that
exists is a new major version. A newer add-on on an older host loads only if everything it
imports is there, and is otherwise refused at load with the version it was built for, the
host's and what is missing. See
[How the plugin world grows](../design.md#how-the-plugin-world-grows).
