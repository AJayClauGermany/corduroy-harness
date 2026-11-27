# corduroy-plugin (Python)

Write a corduroy plugin as decorated functions and build it into a WebAssembly component that
imports only the capabilities you name.

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

```bash
python3 -m venv .venv && .venv/bin/pip install -e .
```

```bash
.venv/bin/corduroy-py build counter.py --out counter.wasm --capabilities state,log
```

`examples/notes-py` in the repository is a complete plugin; `scripts/guests.sh` builds it.

## What a plugin can register

| Decorator | Becomes |
| --- | --- |
| `@plugin.tool(name=None, description=, input=, read_only=, replay_safe=, result_tokens=, result_keep=)` | a tool; `result_tokens` is the most one result should take of the model's context and `result_keep` which part of a longer one the host keeps (`head`, `tail` or `ends`); the function takes the parsed input and returns text, anything else as JSON, or content blocks (below) |
| `@plugin.on_activate` | called with the parsed configuration when the plugin starts, and again after a trap |
| `@plugin.on("turn/end", "tool/*")` | called with each matching event |
| `@plugin.prompt` | text for the system prompt, or `None` |
| `@plugin.gate("tool/call")` | a gate returning `allow()`, `ask(reason)` or `deny(reason)`; one that raises denies |
| `@plugin.service("name", "method")` | a service method taking the parsed payload and the call |

Raise `ToolError(kind, message)` (`invalid-input`, `failed`, `denied`, `unknown-tool`) or
`CallError(kind, message)` to choose the error; any other exception becomes `failed`, and a
host call's `denied` keeps its kind.

## Content blocks

A tool's result is a list of content blocks, text or images. Whatever a tool returns is one
block of text, unless it is a `Text` or an `Image`, or a non-empty list of them:

```python
from corduroy_plugin import Image, Text

@plugin.tool(description="Draw the sales chart.")
def chart(input):
    return [Text("sales by month"), Image("image/png", draw_png())]
```

`Image(mime_type, data)` takes the image's bytes, not base64. A list of anything else is
still JSON. A plugin that calls another tool through the `tools` capability gets its blocks
as the host's own types (`Content_Text`, `Content_Image`), which a tool may return as they
are; `text_of(blocks)` is their text, with an image as a marker such as
`[image: image/png, 1234 bytes]`.

## Capabilities

Each `wit_world.imports.<capability>` module is one capability: `fs_read`, `fs_write`,
`kernel`, `events`, `log`, `clock`, `random`, `tools`, `state`, `service`, `plugins`, `llm`,
`secret`, `http`, `log_read`, `session` (named with hyphens on the command line). A Python component
imports every interface of the world it is built against, so `--capabilities` is the
plugin's declaration of what it needs: the build generates a world with exactly those
imports, and the host instantiates the plugin only if it granted all of them.

## What the SDK routes for you

Built with `--stub-wasi`, as corduroy requires, most of the standard library's contact with
the outside world traps. The SDK replaces it:

| Python | Goes to | Without the capability |
| --- | --- | --- |
| `print`, `sys.stdout`, `sys.stderr` | `log`, as `plugin/log` events | dropped |
| `time.time()`, `datetime.now()`, `time.gmtime()` and friends | `clock` (wall) | raises, saying what to grant |
| `time.monotonic()`, `time.perf_counter()` | `clock` (monotonic) | raises |
| `random`, `os.urandom`, `secrets`, `uuid.uuid4` | `random` | raises |
| `open()` | always raises: use `fs-read` and `fs-write` | |
| `time.sleep()` | always raises: a plugin's call holds up the harness | |

`datetime` is the pure-Python implementation inside a plugin, so that it reads the routed
clock. These run when called, never while the component is being built, so do not call them
at the top level of a module.
