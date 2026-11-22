# Guest languages: spike results

Step 8 of the [order of work](design.md#order-of-work), run early (November 2026) so that its
answers can shape steps 4 to 7. One JavaScript and one Python plugin were built against
`wit/corduroy.wit` as it stands, loaded by the unmodified `corduroy-wasm` host, and driven
through `Tools`, `Prompts` and the event stream. Nothing from the spike is in the build; the
sources that matter are reproduced at the end.

| | JavaScript | Python |
| --- | --- | --- |
| Toolchain | `@bytecodealliance/componentize-js` 0.23.0 (StarlingMonkey) | `componentize-py` 0.25.1 (CPython 3.14) |
| WASI-free build | `disableFeatures: ['stdio', 'random', 'clocks', 'http', 'fetch-event']` | `--stub-wasi` |
| Build time | 3.5 s | 4.5 s |
| Component size | 12.0 MB | 18.5 MB |
| Compile at load, one thread (today's host) | 30 s | 25 s |
| Compile at load, `parallel-compilation`, 4 cores | 8 s | 6.5 s |
| Load with wasmtime's `cache` enabled, after the first | 0.6 s | 0.7 s |
| Instantiate and `activate` | 5 ms | 11 ms |
| A tool call | 0.1 to 0.9 ms | 0.1 to 0.6 ms |
| Smallest memory limit that starts | 16 MB (needs 9.1 MB up front) | 16 MB (needs 12.5 MB up front) |
| Fuel for `activate` | between 2 M and 20 M | under 2 M |

For comparison, the Rust plugins are about 95 KB each and compile in well under a second.

## What is settled

**The WASI question: build without it; the host does not change.** Both toolchains can emit
a component whose only imports are `corduroy:plugin/*`. Built with their defaults, both import
WASI (JavaScript 18 interfaces, Python 25, sockets included) and the host refuses them at load
with ``it imports `wasi:io/poll@…`, which no capability provides``, which is the behaviour we
want. The alternative of linking WASI interfaces that refuse everything is not needed and is
dropped.

**The world works from both languages as it is.** `activate`, `call-tool`, `on-event` and
`prompt-part` all work; `result` errors and the `tool-error` and `host-error` variants map to
exceptions on each side; `fs-read` and `events` calls are scope-checked and a refusal comes
back as `denied` and is logged as `capability/denied`, exactly as for a Rust plugin. A
runaway loop ends as a fuel trap. The default limits (2 G fuel, 64 MB) are enough.

**The script host works in both languages.** One interpreter component took a script in its
`config` and ran it (`new Function` in JavaScript, `exec` in Python). The same component file
was loaded twice with different scripts and different grants; each automation got its own
tools and prompt part, and a script that wrote a file succeeded under an `fs-write` scope
and was `denied` without one. Nothing was compiled per automation.

## What was not expected

**1. Imports are the world, not the calls.** A Rust plugin imports only the host functions
it calls, so `requires()` reports what it uses (`pi-tools`: three of four). JavaScript and
Python components import every interface of the world they were built against, used or not:
a plugin calling only `fs-read` and `events` required all four capabilities. Building against
a narrower world (the spike added a `reader` world with two imports) gives the narrow import
list in both languages, and the host accepts it unchanged.

**2. A stubbed WASI call is a trap, not an error.** The plugin cannot catch it, and a trapped
instance is dead until reloaded. What ordinary code does without WASI:

| | JavaScript | Python |
| --- | --- | --- |
| Print (`console.log`, `print`) | silently discarded | **trap** |
| Wall clock (`Date.now()`, `time.time()`) | frozen at the moment the component was built | **trap** |
| Monotonic clock | **trap** (`performance.now()`) | **trap** |
| `Math.random()`, `random.random()` | same sequence in every instance | same sequence in every instance |
| `crypto.getRandomValues`, `os.urandom` | returns bytes, from the same build-time seed | **trap** |
| `fetch`, `open()` | **trap** | **trap** |
| Environment | none | empty |

Determinism holds, which is what [replay](design.md#durability) needs: nothing a plugin can
observe varies between runs. But it holds by being wrong or fatal, and a `print` left in a
Python plugin takes it down. Randomness that looks random and is not is a trap of a different
kind for anything that generates an identifier or a token.

**3. Loading is slow, and it is all compilation.** The engines are 12 and 18 MB of
WebAssembly that Cranelift compiles each time `WasmRuntime::load` is called. The host builds
wasmtime without `parallel-compilation`, and keeps no compiled code between loads or between
processes. Instantiating an already compiled component is milliseconds.

## What follows

- **A cache of compiled components** (*done in step 6*, see
  [Loading plugins](#loading-plugins)) is a precondition for shipping any JavaScript or
  Python plugin, and for the script host. It has two parts. Within a process, a component is compiled
  once and instantiated many times (today each `load` compiles again, so two automations on
  one script host cost two compilations). Across processes, wasmtime's own cache does the
  job: with the `cache` feature and a `Cache` on the engine's `Config`, components are
  covered as well as modules (checked in the pinned 51.0.0, where `compile_component` goes
  through the same cached path), and a later load of either engine took 0.6 to 0.7 seconds,
  in the same process or a new one. It is keyed by the bytes and the engine's version and
  configuration, and needs no `unsafe` in our code, so the workspace's ban stands with no
  exception. The two compiled engines took 25 MB on disk. Two things to respect: the cache
  directory holds machine code the host will run, so it joins the paths no plugin grant can
  reach; and it is the host's choice of directory, never the default in the user's home
  without being asked. Enabling `parallel-compilation` covers the first load. This belongs
  with step 6.
- **`clock` moves up, and a `log` capability is added.** They are the two things every guest
  plugin reaches for and gets a trap or a wrong answer. `log` is diagnostic output that goes
  to the event stream under the plugin's name; `clock` and a `random` source are host calls
  so that their results can be recorded. All three are trivial `Host` implementations.
  *Done;* see [Host calls](#host-calls-log-clock-and-random).
- **Guest SDKs are required, not a convenience.** One package per language that: wraps the
  generated bindings; routes `console`/`sys.stdout` and `print` to `log`; replaces the clock
  and the random source with the host's; turns an uncaught exception in `call-tool` into
  `tool-error::failed` instead of a trap; and carries the build flags, so that an author
  cannot forget `--stub-wasi`.
- **Worlds per capability set.** The rule "a plugin's requirements are its imports" stays
  and stays enforced, but for these languages the author chooses the list by choosing a
  world. The WIT keeps `plugin` (everything) and the SDKs generate a world from a list of
  capabilities, since a world per combination does not scale past the four we have. Every
  capability added to `plugin` is from then on required by every guest plugin built against
  it, which is one more reason for the SDK to generate the world.
- **The script host is granted everything it imports**, with `Scope::none()` for what an
  automation may not use. That is the existing meaning of an empty scope and needs no new
  mechanism. A grant file must make an omitted capability mean exactly this for script-host
  automations.
- **A restart policy for trapped plugins matters more.** In Rust a trap is a bug. In Python
  it is one stray `print`. The restart limit planned for the plugin tree should count traps,
  and a trapped plugin should be re-instantiated on its next use rather than stay dead until
  someone reloads it. *Done* for WebAssembly plugins (`Limits::restarts`).
- **Memory is per instance.** Each JavaScript or Python instance holds 9 to 13 MB before it
  does anything, so twenty automations are a few hundred megabytes. Acceptable; worth
  knowing before sub-agents multiply instances.

## The gate export

Checked in November 2026, before the export was fixed, as step 4 of the order of work asks:
the world gained `export gate: func(point: string, request: string) -> verdict`, the
`verdict` variant (`allow`, `ask(string)`, `deny(string)`), `read-only` in `tool-spec` and
`gates` in `plugin-info`. One JavaScript and one Python plugin, built WASI-free with the
toolchains and flags above, were loaded by the host, registered as gates, and gave each
verdict correctly; their `echo` tool's `read-only` came through.

| | JavaScript | Python |
| --- | --- | --- |
| `allow` | `{tag: 'allow'}` | `types.Verdict_Allow()` |
| `ask` / `deny` | `{tag: 'deny', val: 'reason'}` | `types.Verdict_Deny("reason")` |
| `read-only` in `tool-spec` | `readOnly` | `read_only=` |
| A plugin without `gate` | build fails: *does not export a "gate" function as expected by the world* | build fails: *Can't instantiate abstract class WitWorld without an implementation for abstract method 'gate'* |

Two consequences:

- **Every export is mandatory.** A plugin that guards nothing still has to write a `gate`
  that is never called. Neither toolchain builds a component that would trap instead, which
  is the better failure, but it is boilerplate the SDKs should absorb.
- **Records are renamed; JSON is not.** Record fields reach each language in its own case,
  but the `request` a gate receives is JSON text and keeps its kebab-case keys, so a gate
  reads `request['read-only']`. The host keeps every JSON key it defines in the same case as
  the WIT, so there is one spelling per name.

## Host calls: log, clock and random

Checked in November 2026 when the three interfaces were added. One JavaScript and one Python
plugin called `log.write`, `clock.now`, `clock.monotonic` twice and `random.get-bytes(16)`
from a tool, built WASI-free against the world with all seven imports and loaded by the host
with each granted. The wall clock was the real time, the monotonic clock went forward,
consecutive calls drew different bytes (unlike the build-time seed of the WASI-free
`Math.random`), and the log lines were on the stream as `plugin/log`.

| | JavaScript | Python |
| --- | --- | --- |
| Import | `import { write } from 'corduroy:plugin/log@0.1.0'` | `from wit_world.imports import log, clock, random` |
| A `level` | `'info'` | `log.Level.INFO` |
| `now()` | `{seconds: bigint, nanoseconds: number}` | `clock.Datetime(seconds, nanoseconds)` |
| `monotonic()` | `bigint` | `int` |
| `get-bytes` | `Uint8Array` | `bytes` |
| A host error | throws `ComponentError` with `payload` `{tag, val}`; not `instanceof Error` | raises `Err` with `value` a `HostError_Denied` / `HostError_Failed` |

The random function was first called `bytes`, which in Python hid the built-in of that name
inside the generated module; it is `get-bytes`. The interface is still `random`, so a Python
plugin imports it under another name (`from wit_world.imports import random as host_random`)
to keep the standard library's.

**What an SDK can patch, and what it cannot.** Assigning the language's own entry points at
module top level survives the build and routes them to the host:

| Works | Still wrong without more work |
| --- | --- |
| JavaScript: `console.log`/`warn`, `Date.now`, `Math.random`, `performance.now` | `new Date()` (frozen at build time), `crypto.getRandomValues` (build-time seed) |
| Python: `builtins.print`, `sys.stdout`, `time.time`, `time.time_ns`, `time.monotonic`, seeding `random` | `datetime.datetime.now()` and `os.urandom` still trap, uncatchably |

So the SDKs patch more than the obvious names: JavaScript's `Date` constructor and `crypto`,
Python's `datetime` and `os.urandom`.

## Between plugins: tools, state and service

Checked in November 2026 when `tools`, `state`, `service`, the `call-error` variant and the
`handle-call` export were added. A JavaScript and a Python plugin, loaded side by side, each
provided a service and called a native tool, the other's service (JavaScript to Python and
back) and the other's tool, and kept a value in `state`. The two stores stayed apart under the
same key, and a plugin calling its own service was refused rather than deadlocked.

| | JavaScript | Python |
| --- | --- | --- |
| `handle-call` | `export function handleCall(call)` | `def handle_call(self, call)` |
| The `service-call` record | a plain object: `caller`, `service`, `method`, `payload` | `types.ServiceCall` with the same fields |
| Returning a `call-error` | `throw { tag: 'unknown', val: '…' }` | `raise Err(types.CallError_Unknown("…"))` |
| `state.get` with no value | `undefined` | `None` |
| `tools.list()` | objects with `name`, `inputSchema`, `replaySafe`, `readOnly`, … | `types.ToolSpec` |

Names in the WIT met each language's own:

- `handle-call` first took the caller, service, method and payload as four strings. In Python
  a parameter named `service` hid the imported `service` module, and on the host the
  generated function had too many arguments, so the call is one `service-call` record.
- The state function to drop a key was first `delete`, a JavaScript keyword (the bindings
  export it as `_delete`). It is `remove`, as in `fs-write`.
- Python's generated modules define `tools.list` and `state.set`, which shadow the builtins
  inside those modules only. Importing the module (`from wit_world.imports import tools`)
  is safe; importing the function by name is not.
- A Python error's class name (`ToolError_InvalidInput`) is not its WIT tag
  (`invalid-input`); an SDK should map one to the other for messages.

## Loading plugins

Checked in November 2026 with step 6. A JavaScript and a Python plugin holding `plugins`
each loaded the Rust `approval` plugin as a child through the host, which started it once
the tree settled. Both build against the `plugins` interface with no surprises: the records
are object literals in JavaScript and `plugins.Grant` / `plugins.LoadRequest` in Python, and
no name clashes with either language.

`load-request` takes a path, not the component's bytes. `fs-read` returns UTF-8 text only,
so a guest could not have read a `.wasm` file to pass on, and the components are 12 to 18 MB
against a 64 MB instance; the host reads the file instead, if the loader's `fs-read` scope
reaches it. A guest that needs binary files for another reason still has no way to read them.

With the compiled-code cache and `parallel-compilation` on (four cores):

| | JavaScript | Python |
| --- | --- | --- |
| First compile | 5.7 s (was 30 s) | 4.3 s (was 25 s) |
| Same component again, same process | 11 ms | 17 ms |
| A new process, from the disk cache | 0.14 s | 0.15 s |

The disk cache took 25 MB for both.

## Reaching out: llm, secret, http and log-read

Checked in November 2026 with step 7. A JavaScript and a Python plugin each asked a model
through `llm` (and were refused one their grant did not name), opened a credential through
`secret`, sent it as a header through `http` to a fake client the host registered for
`https`, and read the event stream through `log-read`, getting back only the session its
grant reached. In both, the client received the secret's value, the plugin did not, and the
value appeared nowhere on the event stream.

The `credential` resource is the first resource in the world, and the first `borrow`. Both
languages handle it without help:

| | JavaScript | Python |
| --- | --- | --- |
| `secret.get` returns | an object with a `name()` method | `types.Credential`, with `name()` |
| A borrowed handle in a variant | `{ tag: 'secret', val: credential }` | `http.HeaderValue_Secret(credential)` |
| `option<list<u8>>` body | a `Uint8Array`, or `undefined` | `bytes`, or `None` |
| `u64` (`after`, an event's `id`) | a `BigInt` (`0n`) | `int` |

A guest must keep the credential alive until the request that borrows it is sent; a
dropped handle is refused with `failed`. `llm.complete` and `http.send` block the plugin's
call until the answer comes back, up to `Limits::outbound_wait` (two minutes by default).

## Not tested

- Async exports, which both toolchains support and which gates, waits and plugin-contributed
  operations may want.
- Third-party packages (npm modules, wheels with native code).
- TypeScript, which is a bundling step in front of the same toolchain.
- Fuel and memory for a realistic workload rather than a string operation.

## Reproducing it

```bash
npm install @bytecodealliance/componentize-js @bytecodealliance/jco
```

```bash
pip install componentize-py
```

JavaScript is built through the API. `witPath` pointing at a directory failed in this
environment, so the WIT is passed as text:

```js
import { componentize } from '@bytecodealliance/componentize-js';
import { readFile, writeFile } from 'node:fs/promises';

const { component } = await componentize(await readFile('plugin.js', 'utf8'), {
  witWorld: await readFile('wit/corduroy.wit', 'utf8'),
  worldName: 'plugin',
  disableFeatures: ['stdio', 'random', 'clocks', 'http', 'fetch-event'],
});
await writeFile('plugin.wasm', component);
```

```bash
componentize-py -d wit -w plugin componentize app --stub-wasi -o plugin.wasm
```

`npx jco wit plugin.wasm` prints a component's imports and exports.

The JavaScript script host, complete:

```js
import { read, list } from 'corduroy:plugin/fs-read@0.1.0';
import { write, remove } from 'corduroy:plugin/fs-write@0.1.0';
import { exec } from 'corduroy:plugin/kernel@0.1.0';
import { emit } from 'corduroy:plugin/events@0.1.0';

const host = { read, list, write, remove, exec, emit };
let script;

export function activate(config) {
  const cfg = JSON.parse(config);
  script = new Function('host', cfg.script)(host);
  return {
    name: cfg.name,
    version: '0.0.0',
    tools: script.tools ?? [],
    subscriptions: script.subscriptions ?? [],
    prompt: !!script.prompt,
  };
}

export function callTool(name, input) {
  try {
    return String(script.call(name, JSON.parse(input)));
  } catch (e) {
    const p = e.payload ?? e;
    throw { tag: p.tag === 'denied' ? 'denied' : 'failed', val: String(p.val ?? e) };
  }
}

export function onEvent(event) { script.onEvent?.(event); }
export function promptPart() { return script.prompt?.(); }
```

The Python one, complete:

```python
import json
import wit_world
from wit_world.imports import types, fs_read, fs_write, kernel, events
from componentize_py_types import Err

script = {}

class WitWorld(wit_world.WitWorld):
    def activate(self, config):
        cfg = json.loads(config)
        host = {"fs_read": fs_read, "fs_write": fs_write, "kernel": kernel, "events": events}
        exec(cfg["script"], host, script)
        tools = [
            types.ToolSpec(name=n, description=d, input_schema='{"type":"object"}', replay_safe=False)
            for n, d in script.get("TOOLS", {}).items()
        ]
        return types.PluginInfo(
            name=cfg["name"], version="0.0.0", tools=tools,
            subscriptions=script.get("SUBSCRIPTIONS", []), prompt="prompt" in script,
        )

    def call_tool(self, name, input):
        try:
            return str(script["call"](name, json.loads(input)))
        except Err as e:
            v = e.value
            if isinstance(v, types.HostError_Denied):
                raise Err(types.ToolError_Denied(v.value))
            raise Err(types.ToolError_Failed(str(v)))
        except Exception as e:
            raise Err(types.ToolError_Failed(f"{type(e).__name__}: {e}"))

    def on_event(self, event):
        if "on_event" in script:
            script["on_event"](event)

    def prompt_part(self):
        return script["prompt"]() if "prompt" in script else None
```

An automation is then a plugin whose `config` is, for instance:

```json
{"name": "count", "script": "return { tools: [{ name: 'count', description: 'Length of a file', inputSchema: '{}', replaySafe: true }], call: (name, args) => host.read(args.path).length }"}
```
