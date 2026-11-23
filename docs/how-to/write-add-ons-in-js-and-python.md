# Write an add-on in JavaScript, TypeScript or Python

Each language has an SDK that builds a plugin into a WebAssembly component importing only the
capabilities you list. The SDK READMEs are the reference for each:
[`sdk/js`](../../sdk/js/README.md), [`sdk/python`](../../sdk/python/README.md).

You need Node.js 20+ for JavaScript and Python 3.11+ for Python. Building compiles a whole
interpreter into the component, so the first build takes tens of seconds; the `corduroy`
command caches the compiled result, so later starts are fast.

## JavaScript or TypeScript

Write the plugin as an object:

```js
import { get, set } from 'corduroy:plugin/state@0.1.0';
import { ToolError, definePlugin } from 'corduroy-plugin';

export default definePlugin({
  name: 'counter',
  version: '0.1.0',
  tools: {
    bump: {
      description: 'Add one to a named counter and return it.',
      input: { type: 'object', properties: { name: { type: 'string' } }, required: ['name'] },
      run({ name }) {
        if (!name) throw new ToolError('invalid-input', 'name is required');
        const next = Number(get(name) ?? '0') + 1;
        set(name, String(next));
        console.log(`${name} is now ${next}`);   // recorded as plugin/log
        return next;
      },
    },
  },
});
```

Build it from the SDK's directory:

```bash
cd sdk/js && npm install
```

```bash
node bin/corduroy-js.js build /path/to/counter.js --out /path/to/counter.wasm --capabilities state,log
```

## Python

Write the plugin as decorated functions:

```python
from corduroy_plugin import Plugin, ToolError
from wit_world.imports import state

plugin = Plugin("counter", "0.1.0")

@plugin.tool(
    description="Add one to a named counter and return it.",
    input={"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
)
def bump(input):
    if not input.get("name"):
        raise ToolError("invalid-input", "name is required")
    count = int(state.get(input["name"]) or 0) + 1
    state.set(input["name"], str(count))
    print(f"{input['name']} is now {count}")   # recorded as plugin/log
    return count
```

Build it with the SDK installed in a virtual environment:

```bash
cd sdk/python && python3 -m venv .venv && .venv/bin/pip install -e .
```

```bash
.venv/bin/corduroy-py build /path/to/counter.py --out /path/to/counter.wasm --capabilities state,log
```

## List the right capabilities

`--capabilities` is the add-on's declaration of what it needs: the component imports exactly
those, and the agent must grant each. Importing a module not on the list fails the build.

The SDKs route the language's own clock, randomness and printing to host calls, so they need
capabilities too:

| You use | List |
| --- | --- |
| `console.log`, `print` | `log` (without it, output is dropped) |
| `Date`, `Date.now()`, `time.time()`, `datetime.now()` | `clock` |
| `performance.now()`, `time.monotonic()` | `clock` |
| `Math.random()`, `crypto.getRandomValues()`, `random`, `uuid.uuid4()` | `random` |

Without the capability these throw, saying what to grant. Files go through `fs-read` and
`fs-write` (`open()` is unavailable), and HTTP through `http` (`fetch` is unavailable). Call
none of these at the top level of a module: they work only once the add-on runs.

## Plug it in

```toml
[[plugins]]
name = "counter"
path = "/path/to/counter.wasm"
grants = { state = "*", log = "*" }
```

then `corduroy check`. A relative `path` is also looked for in `CORDUROY_PLUGIN_PATH`. See
[Grant an add-on what it needs](grant-capabilities.md).

## Beyond tools

Both SDKs can also make an add-on a gate (`gates` / `@plugin.gate`), an event subscriber
(`subscriptions` and `onEvent` / `@plugin.on`), a source of prompt text (`prompt` /
`@plugin.prompt`) and a service (`services` / `@plugin.service`). See
[Write a gate](write-a-gate.md) and [Share a service](share-a-service.md).

[`examples/notes-js`](../../examples/notes-js/plugin.js) and
[`examples/notes-py`](../../examples/notes-py/notes.py) are complete add-ons, built by
`scripts/guests.sh`.
