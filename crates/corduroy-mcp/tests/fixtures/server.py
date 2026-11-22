"""A small MCP server over standard input and output, for the tests: newline-delimited
JSON-RPC, the handshake, tools/list and tools/call. Tools: echo (read-only), add, fail, where,
chart (text, an image, then two more texts).
"""

import json
import os
import sys

TOOLS = [
    {
        "name": "echo",
        "description": "Say the text back",
        "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}},
        "annotations": {"readOnlyHint": True},
    },
    {
        "name": "add",
        "description": "Add two numbers",
        "inputSchema": {"type": "object", "properties": {"a": {"type": "number"}, "b": {"type": "number"}}},
        "annotations": {"idempotentHint": True},
    },
    {"name": "fail", "description": "Always fails", "inputSchema": {"type": "object"}},
    {"name": "where", "description": "The working directory and GREETING", "inputSchema": {"type": "object"}},
    {
        "name": "chart",
        "description": "A caption, a picture and two notes",
        "inputSchema": {"type": "object"},
        "annotations": {"readOnlyHint": True},
    },
]

# The eight bytes every PNG file starts with, in base64 as MCP carries images.
PNG = "iVBORw0KGgo="


def text(value, error=False):
    return {"content": [{"type": "text", "text": value}], "isError": error}


def answer(message):
    method = message.get("method")
    params = message.get("params") or {}
    if method == "initialize":
        return {
            "protocolVersion": params.get("protocolVersion", "2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fixture", "version": "0.1.0"},
        }
    if method == "tools/list":
        return {"tools": TOOLS}
    if method == "ping":
        return {}
    if method == "tools/call":
        name = params.get("name")
        args = params.get("arguments") or {}
        if name == "echo":
            return text(args.get("text", ""))
        if name == "add":
            return text(str(args.get("a", 0) + args.get("b", 0)))
        if name == "fail":
            return text("boom", error=True)
        if name == "where":
            return text(f"{os.getcwd()} {os.environ.get('GREETING', '')}")
        if name == "chart":
            return {
                "content": [
                    {"type": "text", "text": "sales by month"},
                    {"type": "image", "data": PNG, "mimeType": "image/png"},
                    {"type": "text", "text": "up in May"},
                    {"type": "text", "text": "down in June"},
                ],
                "isError": False,
            }
    raise KeyError(method)


for line in sys.stdin:
    try:
        message = json.loads(line)
    except ValueError:
        continue
    if "id" not in message:
        continue  # a notification
    try:
        reply = {"jsonrpc": "2.0", "id": message["id"], "result": answer(message)}
    except KeyError as missing:
        reply = {"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32601, "message": f"unknown {missing}"}}
    sys.stdout.write(json.dumps(reply) + "\n")
    sys.stdout.flush()
