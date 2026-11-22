# corduroy-plugin (JavaScript and TypeScript)

Write a corduroy plugin as a plain object and build it into a WebAssembly component that
imports only the capabilities you name.

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

```bash
npm install
```

```bash
node bin/corduroy-js.js build counter.js --out counter.wasm --capabilities state,log
```

`examples/notes-js` in the repository is a complete plugin; `scripts/guests.sh` builds it.

## What the plugin object can have

| Field | Becomes |
| --- | --- |
| `name`, `version` | the plugin's name and version |
| `activate(config)` | called with the parsed configuration when the plugin starts, and again after a trap |
| `tools: { name: { description, input, readOnly, replaySafe, run(input) } }` | tools for the model; `run` returns text, anything else as JSON, or content blocks (below) |
| `subscriptions`, `onEvent(event)` | events whose topic matches one of the globs |
| `prompt()` | text for the system prompt, or `undefined` |
| `gates: { 'tool/call': (request) => 'allow' \| { ask } \| { deny } }` | a gate; one that throws denies |
| `services: { name: { method(payload, call) } }` | services other plugins call |

Throw `ToolError(kind, message)` (`invalid-input`, `failed`, `denied`, `unknown-tool`) or
`CallError(kind, message)` to choose the error; any other exception becomes `failed`, and a
host call's `denied` keeps its kind.

## Content blocks

A tool's result is a list of content blocks, text or images. Whatever `run` returns is one
block of text, unless it is a block made with `Content`, or a non-empty array of them:

```js
import { Content, definePlugin } from 'corduroy-plugin';

export default definePlugin({
  name: 'charts',
  tools: {
    chart: {
      description: 'Draw the sales chart.',
      run() {
        return [Content.text('sales by month'), Content.image('image/png', drawPng())];
      },
    },
  },
});
```

`Content.image(mimeType, data)` takes the image's bytes (a `Uint8Array`, an `ArrayBuffer` or
an array of numbers), not base64. An array of anything else is still JSON. A plugin that
calls another tool through the `tools` capability gets its blocks, as
`{ tag: 'text', val }` and `{ tag: 'image', val: { mimeType, data } }`;
`Content.textOf(blocks)` is their text, with an image as a marker such as
`[image: image/png, 1234 bytes]`.

## Capabilities

Each `corduroy:plugin/<capability>@0.1.0` module is one capability: `fs-read`, `fs-write`,
`kernel`, `events`, `log`, `clock`, `random`, `tools`, `state`, `service`, `plugins`, `llm`,
`secret`, `http`, `log-read`. A JavaScript component imports every interface of the world it
is built against, so `--capabilities` is the plugin's declaration of what it needs: the
build generates a world with exactly those imports, and the host instantiates the plugin
only if it granted all of them. Importing a module not on the list fails the build.

## What the SDK routes for you

Built without WASI, as corduroy requires, a component's globals do not work. The SDK
replaces them:

| Global | Goes to | Without the capability |
| --- | --- | --- |
| `console.*` | `log`, as `plugin/log` events | dropped |
| `Date`, `Date.now()` | `clock` (wall) | throws, saying what to grant |
| `performance.now()` | `clock` (monotonic) | throws |
| `Math.random()`, `crypto.getRandomValues()` | `random` | throws |
| `fetch` | always throws: use `http` | |

These run when called, never while the component is being built, so do not call them at
the top level of a module.
