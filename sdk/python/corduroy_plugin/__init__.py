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
"""

from collections.abc import Callable
from fnmatch import fnmatchcase
from typing import Any

__all__ = ["CallError", "Plugin", "ToolError", "allow", "ask", "deny"]

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

    def __init__(self, run, name, description, input, read_only, replay_safe):
        self.run = run
        self.name = name
        self.description = description
        self.input = input if input is not None else {"type": "object"}
        self.read_only = read_only
        self.replay_safe = replay_safe


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
    ):
        """Offer the decorated function to the model as a tool. It takes the parsed input
        object and returns text, or anything else as JSON."""

        def register(run):
            tool_name = name or run.__name__
            self.tools[tool_name] = Tool(
                run, tool_name, description or (run.__doc__ or "").strip(), input, read_only, replay_safe
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
