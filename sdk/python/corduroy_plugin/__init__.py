"""Write corduroy plugins in Python.

A plugin module holds a `Plugin` and registers its parts with decorators::

    from corduroy_plugin import Plugin, ToolError

    plugin = Plugin("hello", "0.1.0")

    @plugin.tool(description="Greet someone", read_only=True)
    def hello(input):
        if not input.get("who"):
            raise ToolError("invalid-input", "who is required")
        return f"hello {input['who']}"

`corduroy-py build hello.py --out hello.wasm` turns it into a component. Host capabilities
are the `wit_world.imports` modules, for example ``from wit_world.imports import state``,
and the plugin must be built with each one it imports (``--capabilities state,...``).

A tool's result is a list of content blocks. What a tool returns is one block of text,
unless it is a `Text` or an `Image`, or a list of them::

    from corduroy_plugin import Image, Text

    @plugin.tool(description="Draw the sales chart")
    def chart(input):
        return [Text("sales by month"), Image("image/png", draw_png())]
"""

from collections.abc import Callable, Iterable
from dataclasses import dataclass
from fnmatch import fnmatchcase
from typing import Any

__all__ = [
    "CallError",
    "Image",
    "Plugin",
    "Text",
    "ToolError",
    "allow",
    "ask",
    "deny",
    "text_of",
]

TOOL_ERRORS = ("unknown-tool", "invalid-input", "failed", "denied")
CALL_ERRORS = ("unknown", "invalid-input", "failed", "denied")


class ToolError(Exception):
    """A tool failed. `kind` is one of `invalid-input`, `failed`, `denied`, `unknown-tool`."""

    def __init__(self, kind: str, message: str):
        if kind not in TOOL_ERRORS:
            raise ValueError(f"unknown tool error kind {kind!r}")
        super().__init__(message)
        self.kind = kind
        self.message = message


class CallError(Exception):
    """A service call failed. `kind` is one of `unknown`, `invalid-input`, `failed`, `denied`."""

    def __init__(self, kind: str, message: str):
        if kind not in CALL_ERRORS:
            raise ValueError(f"unknown call error kind {kind!r}")
        super().__init__(message)
        self.kind = kind
        self.message = message


@dataclass(frozen=True)
class Text:
    """A block of text in a tool's result."""

    text: str


@dataclass(frozen=True)
class Image:
    """An image in a tool's result: what kind it is, such as ``image/png``, and its bytes as
    they would be in a file (not base64)."""

    mime_type: str
    data: bytes


def text_of(blocks: Iterable[Any]) -> str:
    """The text of a tool's result, such as `call` of the `tools` capability returns: its
    text blocks joined by newlines, with an image standing as a marker such as
    ``[image: image/png, 1234 bytes]``. Takes the host's blocks, or `Text` and `Image`."""
    lines = []
    for block in blocks:
        # The host's blocks wrap what they hold: a string, or an image.
        held = getattr(block, "value", block)
        if isinstance(held, str):
            lines.append(held)
        elif isinstance(held, Text):
            lines.append(held.text)
        else:
            lines.append(f"[image: {held.mime_type}, {len(held.data)} bytes]")
    return "\n".join(lines)


def allow() -> tuple[str, str]:
    """A gate's answer: let it happen."""
    return ("allow", "")


def ask(reason: str) -> tuple[str, str]:
    """A gate's answer: let it happen once a person has agreed."""
    return ("ask", reason)


def deny(reason: str) -> tuple[str, str]:
    """A gate's answer: refuse it, telling the model why."""
    return ("deny", reason)


class Tool:
    """A tool registered with `Plugin.tool`."""

    def __init__(
        self, run, name, description, input, read_only, replay_safe, result_tokens=None, result_keep="ends"
    ):
        self.run = run
        self.name = name
        self.description = description
        self.input = input if input is not None else {"type": "object"}
        self.read_only = read_only
        self.replay_safe = replay_safe
        self.result_tokens = result_tokens
        self.result_keep = result_keep

    def more(self) -> list[tuple[str, str]]:
        """What the tool says about itself beyond the record's own fields, by name."""
        if not self.result_tokens:
            return []
        return [("result-tokens", str(int(self.result_tokens))), ("result-keep", self.result_keep)]


class Plugin:
    """What a plugin contributes. Create one per plugin module and decorate its parts."""

    def __init__(self, name: str, version: str = "0.0.0"):
        self.name = name
        self.version = version
        self.tools: dict[str, Tool] = {}
        self.listeners: list[tuple[str, Callable]] = []
        self.prompt_fn: Callable | None = None
        self.activate_fn: Callable | None = None
        self.gates: dict[str, Callable] = {}
        self.services: dict[str, dict[str, Callable]] = {}

    def tool(
        self,
        name: str | None = None,
        *,
        description: str = "",
        input: dict | None = None,
        read_only: bool = False,
        replay_safe: bool = False,
        result_tokens: int | None = None,
        result_keep: str = "ends",
    ):
        """Offer the decorated function to the model as a tool. It takes the parsed input
        object and returns text, or anything else as JSON; or content blocks, as a `Text` or
        an `Image` or a list of them, which is how a tool returns an image.

        `result_tokens` is the most one result should take of the model's context, in tokens
        counted as three bytes of text, and `result_keep` which part of a longer one is kept:
        `head`, `tail` or `ends` (both, with the middle left out). The host cuts a longer
        result, and holds a call to less for a model that holds little."""

        def register(run):
            tool_name = name or run.__name__
            self.tools[tool_name] = Tool(
                run,
                tool_name,
                description or (run.__doc__ or "").strip(),
                input,
                read_only,
                replay_safe,
                result_tokens,
                result_keep,
            )
            return run

        return register

    def on_activate(self, run: Callable[[Any], None]):
        """Call the decorated function with the parsed configuration when the plugin starts,
        and again after a trap."""
        self.activate_fn = run
        return run

    def on(self, *topics: str):
        """Call the decorated function with each event whose topic matches one of `topics`
        (`*` matches any run of characters)."""

        def register(run):
            for topic in topics:
                self.listeners.append((topic, run))
            return run

        return register

    def prompt(self, run: Callable[[], str | None]):
        """Ask the decorated function for text to add to the system prompt before each model
        request; `None` adds nothing."""
        self.prompt_fn = run
        return run

    def gate(self, point: str):
        """Make the decorated function a gate at `point`, such as `tool/call`. It takes the
        parsed request and returns `allow()`, `ask(reason)` or `deny(reason)`; one that
        raises denies."""

        def register(run):
            self.gates[point] = run
            return run

        return register

    def service(self, name: str, method: str | None = None):
        """Answer calls to `method` (by default the function's name) of the service `name`.
        The decorated function takes the parsed payload and the call."""

        def register(run):
            self.services.setdefault(name, {})[method or run.__name__] = run
            return run

        return register

    def subscriptions(self) -> list[str]:
        """The topic patterns the plugin listens to, each once."""
        return list(dict.fromkeys(topic for topic, _ in self.listeners))

    def listeners_for(self, topic: str) -> list[Callable]:
        """The listeners an event with `topic` goes to."""
        return [run for pattern, run in self.listeners if fnmatchcase(topic, pattern)]
