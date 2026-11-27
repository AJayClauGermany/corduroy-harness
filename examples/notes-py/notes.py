"""A notes plugin in Python: the model keeps short notes that outlive the process.

Built with: corduroy-py build examples/notes-py/notes.py --out notes_py.wasm \
              --capabilities state,log,clock,random

Notes live in the plugin's `state` store, so they survive a restart of the plugin and of the
harness. `print`, `datetime.now()` and `random` work as usual: the SDK routes them to the
`log`, `clock` and `random` capabilities.
"""

import json
import random
from datetime import datetime, timezone

from corduroy_plugin import Plugin, ToolError
from wit_world.imports import state

PREFIX = "note/"

plugin = Plugin("notes-py", "0.1.0")


def note_keys() -> list[str]:
    return [key for key in state.keys() if key.startswith(PREFIX)]


@plugin.tool(
    description="Save a short note for later. Returns its id.",
    input={"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
)
def note_add(input):
    text = input.get("text")
    if not isinstance(text, str) or not text.strip():
        raise ToolError("invalid-input", "text must be a non-empty string")
    note_id = f"{random.getrandbits(32):08x}"
    stamp = datetime.now(timezone.utc).isoformat()
    state.set(PREFIX + note_id, json.dumps({"text": text, "at": stamp}))
    print(f"saved note {note_id}")
    return note_id


# A long list is cut from its end: the oldest notes are the ones shown.
@plugin.tool(
    description="List every saved note, oldest first.",
    read_only=True,
    replay_safe=True,
    result_tokens=4096,
    result_keep="head",
)
def note_list(_input):
    notes = [
        {"id": key[len(PREFIX):], **json.loads(state.get(key))}
        for key in note_keys()
    ]
    return sorted(notes, key=lambda note: note["at"])


@plugin.tool(
    description="Remove a note by id.",
    input={"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]},
)
def note_remove(input):
    note_id = input.get("id", "")
    if state.get(PREFIX + note_id) is None:
        raise ToolError("invalid-input", f"no note {note_id}")
    state.remove(PREFIX + note_id)
    return f"removed {note_id}"


@plugin.prompt
def prompt():
    count = len(note_keys())
    return None if count == 0 else f"You have {count} saved note(s); note_list shows them."
