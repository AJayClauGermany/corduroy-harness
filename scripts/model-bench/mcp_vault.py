"""A one-tool MCP server over standard input and output, for the bench: `open_shelf` gives
back what is on a numbered shelf. Only this process knows what that is (it is worked out from
VAULT_KEY, which the trial sets), so the model can only learn it by calling the tool.
"""

import hashlib
import json
import os
import sys

TOOLS = [
    {
        "name": "open_shelf",
        "description": "Open one shelf of the vault and say what is on it",
        "inputSchema": {
            "type": "object",
            "properties": {"shelf": {"type": "integer", "description": "The shelf's number"}},
            "required": ["shelf"],
        },
        "annotations": {"readOnlyHint": True},
    }
]


def contents(shelf):
    key = os.environ.get("VAULT_KEY", "")
    return "label-" + hashlib.sha256(f"{key}:{shelf}".encode()).hexdigest()[:8]


def handle(request):
    method = request.get("method")
    if method == "initialize":
        return {
            "protocolVersion": request["params"].get("protocolVersion", "2025-03-26"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "vault", "version": "1"},
        }
    if method == "tools/list":
        return {"tools": TOOLS}
    if method == "tools/call":
        arguments = request["params"].get("arguments") or {}
        try:
            shelf = int(arguments.get("shelf"))
        except (TypeError, ValueError):
            return {"content": [{"type": "text", "text": "shelf must be a number"}], "isError": True}
        return {"content": [{"type": "text", "text": contents(shelf)}], "isError": False}
    return {}


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    message = json.loads(line)
    if "id" not in message:
        continue
    reply = {"jsonrpc": "2.0", "id": message["id"], "result": handle(message)}
    sys.stdout.write(json.dumps(reply) + "\n")
    sys.stdout.flush()
