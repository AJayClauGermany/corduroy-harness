"""The part of a Python plugin that runs inside the component.

Built without WASI (`--stub-wasi`), as corduroy requires, a component has no standard
output, clock, random source or files: `print`, `time.time()`, `datetime.now()`,
`os.urandom()` and `open()` trap and take the instance down, and `random` repeats the same
sequence in every instance. Importing this module, which the generated entry does before the
plugin's own, routes each of them to a host capability where the plugin was built with it,
and makes the rest raise an exception that says what to grant.
"""

import builtins
import io
import json
import os
import sys

# Before anything imports `datetime`: its C implementation reads the clock where nothing
# can reach it, and the pure-Python one asks `time.time()`, which is routed below.
sys.modules["_datetime"] = None  # type: ignore[assignment]

import random as _random  # noqa: E402
import time as _time  # noqa: E402

from componentize_py_types import Err  # noqa: E402
from wit_world.imports import types  # noqa: E402

from corduroy_plugin import CallError, Image, Plugin, Text, ToolError  # noqa: E402

try:
    from wit_world.imports import log as _log
except ImportError:
    _log = None
try:
    from wit_world.imports import clock as _clock
except ImportError:
    _clock = None
try:
    from wit_world.imports import random as _host_random
except ImportError:
    _host_random = None


def _missing(capability: str, what: str) -> RuntimeError:
    return RuntimeError(
        f"{what} needs the `{capability}` capability, which this plugin was not built with"
    )


def _describe(error: BaseException) -> str:
    if isinstance(error, Err):
        return f"{type(error.value).__name__}: {getattr(error.value, 'value', error.value)}"
    return f"{type(error).__name__}: {error}"


# --- Logging ---------------------------------------------------------------------------------


def _write(level, message: str) -> None:
    if _log is None:
        return
    try:
        _log.write(level, message)
    except Err:
        pass  # A refused log line has nowhere else to go.


class _LogStream(io.TextIOBase):
    """Standard output or error, a line at a time, as `plugin/log` events."""

    def __init__(self, level_name: str):
        self._level_name = level_name
        self._pending = ""

    def writable(self) -> bool:
        return True

    def write(self, text: str) -> int:
        self._pending += text
        *lines, self._pending = self._pending.split("\n")
        for line in lines:
            self._emit(line)
        return len(text)

    def flush(self) -> None:
        if self._pending:
            self._emit(self._pending)
            self._pending = ""

    def _emit(self, line: str) -> None:
        if _log is not None:
            _write(getattr(_log.Level, self._level_name), line)


sys.stdout = _LogStream("INFO")
sys.stderr = _LogStream("ERROR")


def _flush() -> None:
    sys.stdout.flush()
    sys.stderr.flush()


# --- Time ------------------------------------------------------------------------------------


def _time_ns() -> int:
    if _clock is None:
        raise _missing("clock", "The time")
    now = _clock.now()
    return now.seconds * 1_000_000_000 + now.nanoseconds


def _monotonic_ns() -> int:
    if _clock is None:
        raise _missing("clock", "A monotonic clock")
    return _clock.monotonic()


_gmtime, _localtime, _ctime, _asctime, _strftime = (
    _time.gmtime,
    _time.localtime,
    _time.ctime,
    _time.asctime,
    _time.strftime,
)


def _sleep(_seconds: float) -> None:
    raise RuntimeError("a plugin cannot sleep: its call holds up the harness")


_time.time_ns = _time_ns
_time.time = lambda: _time_ns() / 1e9
_time.monotonic_ns = _monotonic_ns
_time.monotonic = lambda: _monotonic_ns() / 1e9
_time.perf_counter_ns = _monotonic_ns
_time.perf_counter = _time.monotonic
_time.process_time_ns = _monotonic_ns
_time.process_time = _time.monotonic
_time.gmtime = lambda secs=None: _gmtime(_time.time() if secs is None else secs)
_time.localtime = lambda secs=None: _localtime(_time.time() if secs is None else secs)
_time.ctime = lambda secs=None: _ctime(_time.time() if secs is None else secs)
_time.asctime = lambda t=None: _asctime(_time.localtime() if t is None else t)
_time.strftime = lambda format, t=None: _strftime(format, _time.localtime() if t is None else t)
_time.sleep = _sleep


# --- Randomness ------------------------------------------------------------------------------


def _urandom(count: int) -> bytes:
    if _host_random is None:
        raise _missing("random", "Randomness")
    out = bytearray()
    while len(out) < count:
        out += _host_random.get_bytes(min(65536, count - len(out)))
    return bytes(out)


class _HostRandom(_random.Random):
    """`random.Random` drawing every bit from the host's secure generator."""

    def seed(self, *_args, **_kwargs) -> None:
        pass

    def random(self) -> float:
        return (int.from_bytes(_urandom(7), "big") >> 3) * 2.0**-53

    def getrandbits(self, k: int) -> int:
        if k < 0:
            raise ValueError("number of bits must be non-negative")
        count = (k + 7) // 8
        return int.from_bytes(_urandom(count), "big") >> (count * 8 - k)

    def randbytes(self, n: int) -> bytes:
        return _urandom(n)

    def getstate(self):
        raise NotImplementedError("the host's generator has no state to save")

    def setstate(self, _state) -> None:
        raise NotImplementedError("the host's generator has no state to restore")


_instance = _HostRandom()
_random._inst = _instance
for _name in dir(_instance):
    if not _name.startswith("_") and hasattr(_random, _name) and callable(getattr(_instance, _name)):
        if _name not in ("Random", "SystemRandom"):
            setattr(_random, _name, getattr(_instance, _name))
_random._urandom = _urandom
os.urandom = _urandom


# --- Files -----------------------------------------------------------------------------------


def _open(*_args, **_kwargs):
    raise PermissionError(
        "a plugin has no files of its own: use the `fs-read` and `fs-write` capabilities"
    )


builtins.open = _open
io.open = _open


# --- The exports -----------------------------------------------------------------------------

_TOOL_ERRORS = {
    "unknown-tool": types.ToolError_UnknownTool,
    "invalid-input": types.ToolError_InvalidInput,
    "failed": types.ToolError_Failed,
    "denied": types.ToolError_Denied,
}
_CALL_ERRORS = {
    "unknown": types.CallError_Unknown,
    "invalid-input": types.CallError_InvalidInput,
    "failed": types.CallError_Failed,
    "denied": types.CallError_Denied,
}


def _host_error_kind(error: Err):
    if isinstance(error.value, types.HostError_Denied):
        return "denied", error.value.value
    if isinstance(error.value, types.HostError_Failed):
        return "failed", error.value.value
    return None


def _as_tool_error(error: BaseException) -> Err:
    if isinstance(error, ToolError):
        return Err(_TOOL_ERRORS[error.kind](error.message))
    if isinstance(error, Err) and (kind := _host_error_kind(error)):
        return Err(_TOOL_ERRORS[kind[0]](kind[1]))
    return Err(types.ToolError_Failed(_describe(error)))


def _as_call_error(error: BaseException) -> Err:
    if isinstance(error, CallError):
        return Err(_CALL_ERRORS[error.kind](error.message))
    if isinstance(error, Err) and (kind := _host_error_kind(error)):
        return Err(_CALL_ERRORS[kind[0]](kind[1]))
    return Err(types.CallError_Failed(_describe(error)))


def _parse(text: str):
    return json.loads(text) if text.strip() else {}


def _text(value) -> str:
    if value is None:
        return ""
    return value if isinstance(value, str) else json.dumps(value)


# What a tool may return as a block: the SDK's own, or the host's, as `tools.call` gives them.
_BLOCKS = (Text, Image, types.Content_Text, types.Content_Image)


def _block(block):
    if isinstance(block, Text):
        return types.Content_Text(block.text)
    if isinstance(block, Image):
        return types.Content_Image(types.Image(mime_type=block.mime_type, data=bytes(block.data)))
    return block


def _content(value) -> list:
    """What a tool returned, as the content blocks the host passes on: blocks as they are,
    and anything else as one block of text. A list is a list of blocks only if it has
    something in it and all of it is blocks; any other list is JSON, as it always was."""
    blocks = list(value) if isinstance(value, (list, tuple)) else [value]
    if blocks and all(isinstance(block, _BLOCKS) for block in blocks):
        return [_block(block) for block in blocks]
    return [types.Content_Text(_text(value))]


def _verdict(answer):
    kind, reason = answer if isinstance(answer, tuple) else (answer, "")
    if kind == "allow":
        return types.Verdict_Allow()
    if kind == "ask" and reason:
        return types.Verdict_Ask(reason)
    if kind == "deny" and reason:
        return types.Verdict_Deny(reason)
    return types.Verdict_Deny("the gate gave no answer it could stand by")


def _find_plugin(module) -> Plugin:
    found = [value for value in vars(module).values() if isinstance(value, Plugin)]
    if len(found) != 1:
        raise RuntimeError(f"{module.__name__} must hold exactly one Plugin, not {len(found)}")
    return found[0]


def world_for(module):
    """The `WitWorld` class componentize-py exports, for the plugin `module` holds."""
    plugin = _find_plugin(module)

    class WitWorld:
        def activate(self, config: str) -> types.PluginInfo:
            try:
                parsed = _parse(config)
                if plugin.activate_fn is not None:
                    plugin.activate_fn(parsed)
            except Exception as error:  # noqa: BLE001
                raise Err(_describe(error)) from None
            finally:
                _flush()
            return types.PluginInfo(
                name=plugin.name,
                version=plugin.version,
                tools=[
                    types.ToolSpec(
                        name=tool.name,
                        description=tool.description,
                        input_schema=json.dumps(tool.input),
                        replay_safe=tool.replay_safe,
                        read_only=tool.read_only,
                        more=[
                            types.NamedValue(name=name, value=value)
                            for name, value in tool.more()
                        ],
                    )
                    for tool in plugin.tools.values()
                ],
                subscriptions=plugin.subscriptions(),
                prompt=plugin.prompt_fn is not None,
                gates=list(plugin.gates),
                provides=list(plugin.services),
                more=[],
            )

        def call_tool(self, name: str, input: str) -> list:
            tool = plugin.tools.get(name)
            if tool is None:
                raise Err(types.ToolError_UnknownTool(name))
            try:
                try:
                    parsed = _parse(input)
                except ValueError as error:
                    raise ToolError("invalid-input", str(error)) from None
                return _content(tool.run(parsed))
            except BaseException as error:  # noqa: BLE001
                raise _as_tool_error(error) from None
            finally:
                _flush()

        def on_event(self, event: types.Event) -> None:
            for listener in plugin.listeners_for(event.topic):
                try:
                    listener(event)
                except Exception as error:  # noqa: BLE001
                    print(f"listener failed: {_describe(error)}", file=sys.stderr)
            _flush()

        def prompt_part(self):
            if plugin.prompt_fn is None:
                return None
            try:
                part = plugin.prompt_fn()
                return part if isinstance(part, str) else None
            except Exception as error:  # noqa: BLE001
                print(f"prompt failed: {_describe(error)}", file=sys.stderr)
                return None
            finally:
                _flush()

        def gate(self, point: str, request: str):
            decide = plugin.gates.get(point)
            if decide is None:
                return types.Verdict_Deny(f"this plugin does not guard `{point}`")
            try:
                return _verdict(decide(json.loads(request)))
            except Exception as error:  # noqa: BLE001
                # A gate that cannot answer fails closed.
                return types.Verdict_Deny(f"the gate failed: {_describe(error)}")
            finally:
                _flush()

        def handle_call(self, call: types.ServiceCall) -> str:
            method = plugin.services.get(call.service, {}).get(call.method)
            if method is None:
                raise Err(types.CallError_Unknown(f"{call.service}.{call.method}"))
            try:
                try:
                    payload = _parse(call.payload)
                except ValueError as error:
                    raise CallError("invalid-input", str(error)) from None
                return _text(method(payload, call))
            except BaseException as error:  # noqa: BLE001
                raise _as_call_error(error) from None
            finally:
                _flush()

    return WitWorld
