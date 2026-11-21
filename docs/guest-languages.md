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

- **A cache of compiled components** is a precondition for shipping any JavaScript or Python
  plugin, and for the script host. It has two parts. Within a process, a component is compiled
  once and instantiated many times (today each `load` compiles again, so two automations on
  one script host cost two compilations). Across processes, compiled code is stored keyed by
  the component's hash and the engine's version and configuration. wasmtime's
  `Component::deserialize` is `unsafe`, which the host workspace forbids, so this needs a
  decision: wasmtime's own cache if it covers components in the pinned version, or one
  narrowly scoped exception in a crate of its own. Enabling `parallel-compilation` is the
  cheap first step. This belongs with step 6, where the component's hash is already computed
  for `plugin/load`.
- **`clock` moves up, and a `log` capability is added.** They are the two things every guest
  plugin reaches for and gets a trap or a wrong answer. `log` is diagnostic output that goes
  to the event stream under the plugin's name; `clock` and a `random` source are host calls
  so that their results can be recorded. All three are trivial `Host` implementations.
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
  someone reloads it.
- **Memory is per instance.** Each JavaScript or Python instance holds 9 to 13 MB before it
  does anything, so twenty automations are a few hundred megabytes. Acceptable; worth
  knowing before sub-agents multiply instances.

## Not tested

- Async exports, which both toolchains support and which gates, waits and plugin-contributed
  operations may want.
- What either toolchain does when the world gains an export a plugin does not implement.
  Step 4 adds one for gates.
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
