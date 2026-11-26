#!/usr/bin/env python3
"""A spike, not part of the harness: does Sign in with ChatGPT work for us at all?

    scripts/siwc-spike.py login      sign in through the browser and save the tokens
    scripts/siwc-spike.py models     list the models the signed-in account may use
    scripts/siwc-spike.py ask        one streamed request billed to the ChatGPT plan
                                     (--body FILE [--adapt]: a request the harness built;
                                     two are in scripts/siwc-spike-bodies/)
    scripts/siwc-spike.py refresh    exchange the refresh token for a new pair
    scripts/siwc-spike.py logout     revoke the refresh token and delete what was saved

Standard library only (Python 3.9+). Tokens are kept in
~/.config/corduroy/siwc-spike/credentials.json (mode 0600) and are never printed.

What this does not do: verify the ID token's signature (that needs RSA, which the
standard library lacks). It checks the claims only; the real implementation must verify.

Flow: https://developers.openai.com/siwc/token-sharing-open-source
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import http.server
import json
import os
import secrets
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
import webbrowser
from pathlib import Path

AUTHORIZE = "https://auth.openai.com/api/accounts/authorize"
TOKEN = "https://auth.openai.com/api/accounts/oauth/token"
DISCOVERY = "https://auth.openai.com/.well-known/openid-configuration"
ISSUER = "https://auth.openai.com"
RESOURCE = "https://api.openai.com/v1"
SCOPE = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct"
PLAN_SCOPE = "chatgpt.tokens.use.direct"
REGISTRATION_CLIENT = "dynamic_agent_client"
APP_NAME = "Corduroy"
CALLBACK_PATH = "/auth/callback"
USER_AGENT = "corduroy-siwc-spike/0"

HOME = Path.home() / ".config" / "corduroy" / "siwc-spike"
CREDENTIALS = HOME / "credentials.json"
HOST_ID = HOME / "host-id"


class Failed(Exception):
    """Something the person should read; no traceback."""


def say(text: str = "") -> None:
    sys.stdout.write(text + "\n")
    sys.stdout.flush()


def write_private(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    scratch = path.with_suffix(".tmp")
    handle = os.open(scratch, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(handle, "w") as file:
        file.write(text)
    os.replace(scratch, path)


def host_id() -> str:
    if HOST_ID.exists():
        return HOST_ID.read_text().strip()
    made = f"urn:uuid:{uuid.uuid4()}"
    write_private(HOST_ID, made + "\n")
    return made


def saved() -> dict | None:
    return json.loads(CREDENTIALS.read_text()) if CREDENTIALS.exists() else None


def save(record: dict) -> None:
    write_private(CREDENTIALS, json.dumps(record, indent=2) + "\n")


def request(url: str, form: dict | None = None, body: dict | None = None, bearer: str = ""):
    """Open `url`; an HTTP error becomes `Failed` carrying the status and the body."""
    headers = {"User-Agent": USER_AGENT, "Accept": "application/json"}
    data = None
    if form is not None:
        data = urllib.parse.urlencode(form).encode()
        headers["Content-Type"] = "application/x-www-form-urlencoded"
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    if bearer:
        headers["Authorization"] = f"Bearer {bearer}"
    try:
        return urllib.request.urlopen(urllib.request.Request(url, data, headers), timeout=120)
    except urllib.error.HTTPError as error:
        text = error.read().decode(errors="replace")
        request_id = error.headers.get("x-request-id", "")
        raise Failed(f"HTTP {error.code} from {url}\n  request id: {request_id}\n  {text}") from None
    except urllib.error.URLError as error:
        raise Failed(f"could not reach {url}: {error.reason}") from None


def claims(token: str) -> dict:
    """The payload of a JWT, unverified."""
    try:
        part = token.split(".")[1]
        return json.loads(base64.urlsafe_b64decode(part + "=" * (-len(part) % 4)))
    except (IndexError, ValueError):
        raise Failed("the ID token is not a JWT") from None


def wait_for_callback(port: int, seconds: int) -> dict:
    """Serve 127.0.0.1:<port> until the browser comes back; return its query."""
    got: dict = {}

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self) -> None:  # noqa: N802 (the base class's name)
            url = urllib.parse.urlsplit(self.path)
            if url.path != CALLBACK_PATH:
                self.send_error(404)
                return
            got.update({key: values[0] for key, values in urllib.parse.parse_qs(url.query).items()})
            page = b"<p>Corduroy has what it needs. You can close this tab.</p>"
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(page)))
            self.end_headers()
            self.wfile.write(page)

        def log_message(self, *_: object) -> None:
            """Quiet: the query holds the authorization code."""

    try:
        server = http.server.HTTPServer(("127.0.0.1", port), Handler)
    except OSError as error:
        raise Failed(f"cannot listen on 127.0.0.1:{port} ({error}); try --port") from None
    with server:
        server.timeout = 1
        until = time.monotonic() + seconds
        while not got and time.monotonic() < until:
            server.handle_request()
    if not got:
        raise Failed(f"no callback within {seconds}s")
    return got


def keep(tokens: dict, client_id: str, identity: dict) -> dict:
    """The record to save after a code exchange or a refresh."""
    return {
        "email": identity.get("email"),
        "issuer": identity.get("iss"),
        "subject": identity.get("sub"),
        "client_id": client_id,
        "ext_agent_host_id": host_id(),
        "id_token": tokens.get("id_token"),
        "access_token": tokens["access_token"],
        "refresh_token": tokens.get("refresh_token"),
        "token_type": tokens.get("token_type"),
        "expires_in": tokens.get("expires_in", 3600),
        "scopes": str(tokens.get("scope", "")).split(),
        "saved_at": int(time.time()),
    }


def login(args: argparse.Namespace) -> None:
    before = saved()
    client_id = before["client_id"] if before else REGISTRATION_CLIENT
    redirect = f"http://127.0.0.1:{args.port}{CALLBACK_PATH}"
    state, nonce = secrets.token_urlsafe(32), secrets.token_urlsafe(32)
    verifier = secrets.token_urlsafe(64)
    challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest())
    asked = {
        "client_id": client_id,
        "ext_agent_host_id": host_id(),
        "response_type": "code",
        "redirect_uri": redirect,
        "scope": SCOPE,
        "resource": RESOURCE,
        "state": state,
        "nonce": nonce,
        "code_challenge_method": "S256",
        "code_challenge": challenge.rstrip(b"=").decode(),
    }
    if before:
        say(f"signing in again with the registered client {client_id}")
        if before.get("id_token"):
            asked["id_token_hint"] = before["id_token"]
    else:
        say("first sign-in: registering a client (dynamic registration)")
        asked["agent_name_hint"] = APP_NAME
    url = f"{AUTHORIZE}?{urllib.parse.urlencode(asked, quote_via=urllib.parse.quote)}"
    say(f"\nopening the browser; if nothing opens, go to:\n\n  {url}\n")
    webbrowser.open(url)

    back = wait_for_callback(args.port, args.wait)
    say(f"callback parameters: {', '.join(sorted(back))}")
    if back.get("state") != state:
        raise Failed("the callback's state is not the one sent; stopping")
    if "error" in back:
        raise Failed(f"sign-in refused: {back['error']}: {back.get('error_description', '')}")
    if "code" not in back:
        raise Failed("the callback has no code")
    issued = back.get("client_id")
    if before and issued not in (None, client_id):
        raise Failed(f"the callback names another client ({issued}); stopping")
    if not before:
        if not issued or issued == REGISTRATION_CLIENT:
            raise Failed("no client ID was issued: registration did not complete")
        client_id = issued
        say(f"issued client ID: {client_id}")

    tokens = json.load(
        request(
            TOKEN,
            form={
                "grant_type": "authorization_code",
                "client_id": client_id,
                "code": back["code"],
                "code_verifier": verifier,
                "redirect_uri": redirect,
                "resource": RESOURCE,
            },
        )
    )
    say(f"token response fields: {', '.join(sorted(tokens))}")
    say(f"granted scope: {tokens.get('scope')}")
    say(f"expires_in: {tokens.get('expires_in')}")

    identity = claims(tokens.get("id_token", ""))
    checks = {
        "iss is auth.openai.com": identity.get("iss") == ISSUER,
        "aud is the issued client": client_id in (identity.get("aud"), *_list(identity.get("aud"))),
        "nonce is ours": identity.get("nonce") == nonce,
        "not expired": identity.get("exp", 0) > time.time(),
        "has sub": bool(identity.get("sub")),
        "refresh token returned": bool(tokens.get("refresh_token")),
        f"{PLAN_SCOPE} granted": PLAN_SCOPE in str(tokens.get("scope", "")).split(),
    }
    say("\nID token (signature NOT verified by this spike):")
    for name, held in checks.items():
        say(f"  [{'ok' if held else 'FAILED'}] {name}")
    if before and before.get("subject") not in (None, identity.get("sub")):
        raise Failed("this is a different account than the saved one; run logout first")

    save(keep(tokens, client_id, identity))
    say(f"\nsaved to {CREDENTIALS}")
    if not all(checks.values()):
        raise Failed("signed in, but a check above failed")
    say("login works. Next: scripts/siwc-spike.py models, then ask")


def _list(value: object) -> list:
    return value if isinstance(value, list) else []


def refresh(_: argparse.Namespace | None = None) -> dict:
    record = saved()
    if not record or not record.get("refresh_token"):
        raise Failed("nothing to refresh; run login first")
    tokens = json.load(
        request(
            TOKEN,
            form={
                "grant_type": "refresh_token",
                "client_id": record["client_id"],
                "refresh_token": record["refresh_token"],
                "resource": RESOURCE,
            },
        )
    )
    rotated = tokens.get("refresh_token") not in (None, record["refresh_token"])
    tokens.setdefault("refresh_token", record["refresh_token"])
    tokens.setdefault("id_token", record.get("id_token"))
    tokens.setdefault("scope", " ".join(record.get("scopes", [])))
    identity = {"email": record["email"], "iss": record["issuer"], "sub": record["subject"]}
    record = keep(tokens, record["client_id"], identity)
    save(record)
    say(f"refreshed: expires_in {record['expires_in']}, refresh token rotated: {rotated}")
    return record


def access_token() -> str:
    record = saved()
    if not record:
        raise Failed("not signed in; run login first")
    if time.time() > record["saved_at"] + record["expires_in"] - 60:
        record = refresh()
    return record["access_token"]


def models(_: argparse.Namespace) -> None:
    listing = json.load(request(f"{RESOURCE}/models", bearer=access_token()))
    entries = listing.get("models") or listing.get("data") or []
    if not entries:
        say(f"no models; the response's fields were: {', '.join(sorted(listing))}")
    for entry in entries:
        if entry.get("visibility", "list") == "list":
            say(f"  {entry.get('slug') or entry.get('id')}\t{entry.get('display_name', '')}")


def ask(args: argparse.Namespace) -> None:
    if args.body:
        body = json.loads(Path(args.body).read_text())
        if args.adapt:
            body = adapted(body)
    else:
        if not args.model:
            raise Failed("give --model (a slug from `models`) or --body")
        body = {
            "model": args.model,
            "instructions": "Answer in one short sentence.",
            "input": [{"role": "user", "content": args.prompt}],
            "store": False,
            "stream": True,
        }
    ended = None
    with request(f"{RESOURCE}/responses", body=body, bearer=access_token()) as stream:
        say(f"HTTP {stream.status}; response headers: {', '.join(sorted(stream.headers.keys()))}\n")
        for raw in stream:
            line = raw.decode(errors="replace").strip()
            if not line.startswith("data:") or line == "data: [DONE]":
                continue
            event = json.loads(line.removeprefix("data:"))
            kind = event.get("type", "")
            if kind == "response.output_text.delta":
                sys.stdout.write(event.get("delta", ""))
                sys.stdout.flush()
            elif kind in ("response.completed", "response.failed", "response.incomplete", "error"):
                ended = event
    say("\n")
    if not ended:
        raise Failed("the stream ended without response.completed")
    if ended.get("type") != "response.completed":
        raise Failed(f"{ended.get('type')}:\n{json.dumps(ended, indent=2)}")
    response = ended.get("response", {})
    for item in response.get("output", []):
        detail = item.get("name", "")
        if item.get("type") == "function_call":
            detail = f"{item.get('name')} {item.get('arguments')}"
        say(f"output item: {item.get('type')} {detail}")
    usage = response.get("usage") or {}
    counts = {key: value for key, value in usage.items() if isinstance(value, int)}
    say(f"response.completed (id {response.get('id')}); usage: {json.dumps(counts)}")


def adapted(body: dict) -> dict:
    """`body` as the plan-usage preview wants it: no system-role items, no token limit."""
    said, kept = [], []
    for item in body.get("input", []):
        if item.get("role") == "system":
            content = item.get("content")
            parts = content if isinstance(content, list) else [{"text": content}]
            said.extend(str(part.get("text", "")) for part in parts)
        else:
            kept.append(item)
    body = {key: value for key, value in body.items() if key != "max_output_tokens"}
    body["input"] = kept
    if said:
        body["instructions"] = "\n\n".join(said)
    return body


def logout(_: argparse.Namespace) -> None:
    record = saved()
    if record and record.get("refresh_token"):
        endpoint = json.load(request(DISCOVERY)).get("revocation_endpoint")
        if not endpoint:
            raise Failed("the discovery document names no revocation_endpoint; nothing removed")
        request(
            endpoint,
            form={
                "token": record["refresh_token"],
                "token_type_hint": "refresh_token",
                "client_id": record["client_id"],
            },
        ).close()
        say("refresh token revoked")
    CREDENTIALS.unlink(missing_ok=True)
    say(f"removed {CREDENTIALS} (the host ID is kept)")


def main() -> int:
    parser = argparse.ArgumentParser(description=(__doc__ or "").split("\n", maxsplit=1)[0])
    commands = parser.add_subparsers(dest="command", required=True)
    entry = commands.add_parser("login")
    entry.add_argument("--port", type=int, default=1455, help="loopback port (default 1455)")
    entry.add_argument("--wait", type=int, default=300, help="seconds to wait for the browser")
    entry.set_defaults(run=login)
    commands.add_parser("models").set_defaults(run=models)
    entry = commands.add_parser("ask")
    entry.add_argument("prompt", nargs="?", default="Say exactly: Hello, world!")
    entry.add_argument("--model", help="a slug from `models`")
    entry.add_argument("--body", help="send this JSON file as the request body instead")
    entry.add_argument(
        "--adapt",
        action="store_true",
        help="with --body: move system-role items to `instructions`, drop max_output_tokens",
    )
    entry.set_defaults(run=ask)
    commands.add_parser("refresh").set_defaults(run=refresh)
    commands.add_parser("logout").set_defaults(run=logout)
    args = parser.parse_args()
    try:
        args.run(args)
    except Failed as failure:
        sys.stderr.write(f"\nerror: {failure}\n")
        return 1
    except KeyboardInterrupt:
        return 130
    return 0


if __name__ == "__main__":
    sys.exit(main())
