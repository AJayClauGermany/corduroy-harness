#!/usr/bin/env python3
"""An experiment: a tool-calling model (Cactus Compute's Needle 3, 121M parameters) behind
the chat-completions endpoint the bench talks to, so that the same tasks can be put to a
model that is not a language model in the usual sense.

    python3 -m venv ~/.cache/corduroy/needle && ~/.cache/corduroy/needle/bin/pip install cactus-compute==4.2.7
    ~/.cache/corduroy/needle/bin/python scripts/model-bench/needle_server.py --port 8081
    sudo python3 scripts/model-bench/bench.py --tier 1 --label needle3 \\
        --url http://127.0.0.1:8081/v1/chat/completions --out /var/tmp/corduroy-bench/needle3

Such a model is given tools and a request and answers with calls, never with prose. The loop
around it therefore needs two things a language model supplies by itself, and both are made
here, with no change to the harness:

- **An end.** The run ends when the model calls nothing (an empty answer goes back), or when
  it calls `finish`, a tool this server adds to the ones it was sent.
- **An answer.** `finish` takes the answer as its argument, which goes back as the reply's
  text.

The conversation is replayed to the model in the form its own runtime uses: the request,
then each round of tool results as a JSON list. Token counts are estimates (four characters
to a token): the runtime reports none. The runtime fetches its engine and weights on first
use and reports usage to its makers unless told not to, which this does (`CACTUS_TELEMETRY=0`).
"""

from __future__ import annotations

import argparse
import json
import os
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

os.environ.setdefault("CACTUS_TELEMETRY", "0")

FINISH = {
    "name": "finish",
    "description": "Call this when the task is done or the question can be answered. "
    "Give the final answer, with any value that was asked for.",
    "parameters": {
        "type": "object",
        "properties": {"answer": {"type": "string", "description": "The final answer."}},
        "required": ["answer"],
    },
}

LOCK = threading.Lock()
AGENT: dict = {"key": None, "agent": None}


def text_of(content) -> str:
    """A message's content as text: a string, or the text parts of a list."""
    if isinstance(content, str):
        return content
    return "\n".join(
        part.get("text", "") for part in content or [] if isinstance(part, dict)
    )


def turns(messages: list[dict]) -> tuple[str, list[str]]:
    """The system prompt, and what the model is told turn by turn: each thing a person
    said, and each round of tool results as the JSON list its runtime would hand back."""
    system = ""
    told: list[str] = []
    results: list = []
    for message in messages:
        role = message.get("role")
        if role == "tool":
            body = text_of(message.get("content"))
            try:
                results.append(json.loads(body))
            except ValueError:
                results.append(body)
            continue
        if results:
            told.append(json.dumps(results))
            results = []
        if role == "system":
            system += text_of(message.get("content"))
        elif role == "user":
            told.append(text_of(message.get("content")))
    if results:
        told.append(json.dumps(results))
    return system, told


def complete(request: dict) -> dict:
    import cactus  # noqa: PLC0415 (after the environment is set)

    system, told = turns(request.get("messages") or [])
    tools = [
        {
            "name": tool["function"]["name"],
            "description": tool["function"].get("description", ""),
            "parameters": tool["function"].get("parameters") or {"type": "object", "properties": {}},
        }
        for tool in request.get("tools") or []
        if tool.get("type") == "function"
    ] + [FINISH]
    key = json.dumps([system, tools], sort_keys=True)
    with LOCK:
        if AGENT["key"] != key:
            if AGENT["agent"] is not None:
                AGENT["agent"].close()
            AGENT["agent"] = cactus.Cactus(tools=tools, system=system, auto_date=False)
            AGENT["key"] = key
        agent = AGENT["agent"]
        agent.reset()
        response: dict = {}
        for turn in told:
            response = agent._complete(turn, ground=False)  # noqa: SLF001 (no telemetry, no turn count)
    calls = response.get("function_calls") or []
    finish = next((call for call in calls if call.get("name") == "finish"), None)
    if finish is not None or not calls:
        answer = str((finish or {}).get("arguments", {}).get("answer", ""))
        message = {"role": "assistant", "content": answer}
        reason = "stop"
    else:
        stamp = time.monotonic_ns()
        message = {
            "role": "assistant",
            "content": None,
            "tool_calls": [
                {
                    "id": f"call_{stamp}_{index}",
                    "type": "function",
                    "function": {
                        "name": call.get("name"),
                        "arguments": json.dumps(call.get("arguments") or {}),
                    },
                }
                for index, call in enumerate(calls)
            ],
        }
        reason = "tool_calls"
    asked = (len(key) + sum(len(turn) for turn in told)) // 4
    wrote = len(json.dumps(message)) // 4
    return {
        "id": "chatcmpl-needle",
        "object": "chat.completion",
        "created": int(time.time()),
        "model": request.get("model", "needle3"),
        "choices": [{"index": 0, "message": message, "finish_reason": reason}],
        "usage": {"prompt_tokens": asked, "completion_tokens": wrote, "total_tokens": asked + wrote},
    }


class Handler(BaseHTTPRequestHandler):
    def reply(self, status: int, body: dict) -> None:
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def stream(self, body: dict) -> None:
        """The same reply as a stream of three events: what was said, how it ended, usage."""
        choice = body["choices"][0]
        delta = dict(choice["message"])
        for index, call in enumerate(delta.get("tool_calls") or []):
            call["index"] = index
        head = {key: body[key] for key in ("id", "created", "model")}
        head["object"] = "chat.completion.chunk"
        events = [
            {**head, "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
            {**head, "choices": [{"index": 0, "delta": {}, "finish_reason": choice["finish_reason"]}]},
            {**head, "choices": [], "usage": body["usage"]},
        ]
        data = "".join(f"data: {json.dumps(event)}\n\n" for event in events) + "data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(data.encode())))
        self.end_headers()
        self.wfile.write(data.encode())

    def do_GET(self) -> None:  # noqa: N802 (the base class's name)
        self.reply(200, {"status": "ok"})

    def do_POST(self) -> None:  # noqa: N802
        try:
            length = int(self.headers.get("Content-Length") or 0)
            request = json.loads(self.rfile.read(length))
            body = complete(request)
            if request.get("stream"):
                self.stream(body)
            else:
                self.reply(200, body)
        except Exception as error:  # noqa: BLE001 (whatever went wrong, the caller is told)
            self.reply(500, {"error": {"message": f"{type(error).__name__}: {error}"}})

    def log_message(self, *_args) -> None:
        pass


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--port", type=int, default=8081)
    args = parser.parse_args()
    ThreadingHTTPServer(("127.0.0.1", args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
