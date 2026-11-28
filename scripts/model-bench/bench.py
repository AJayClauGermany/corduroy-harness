#!/usr/bin/env python3
"""Drive `corduroy` from outside, as another harness would, through a list of tasks against a
real (small, local) model, several times each, and write down what happened.

    scripts/model-bench/run.sh --models all                         # every known model, unattended
    scripts/local-model.sh start
    cargo build --release -p corduroy-cli && cargo plugins
    sudo python3 scripts/model-bench/bench.py --trials 3            # everything, one model
    sudo python3 scripts/model-bench/bench.py --only read,edit      # some tasks
    python3 scripts/model-bench/bench.py --summary RESULTS.jsonl    # the table again
    python3 scripts/model-bench/bench.py --compare OUT              # the models side by side

The tasks are in `tasks.py`. Each one sets a workspace up, runs the command (`run`, `continue`,
`stop`, a signal) and then checks the outcome from outside: files, exit codes, the event log.

**The agent must not be able to read its own exam.** Its shell kernel is not a sandbox, so run
as root on Linux this drops the agent to an unprivileged user (`--user`, made if missing) and
keeps this directory, every expected value and every hidden test where that user cannot read
them. On macOS the agent runs under `sandbox-exec` with those places denied, which needs no
root. Where neither is to be had there is no such wall, and the summary says so.

Nothing here is a test of the repository (`cargo t` never runs it): it needs a model and
takes hours. Standard library only.
"""

from __future__ import annotations

import argparse
import functools
import json
import os
import platform
import pwd
import re
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import traceback
import urllib.error
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
DEFAULT_URL = "http://127.0.0.1:8080/v1/chat/completions"
LOCAL_MODEL = ROOT / "scripts/local-model.sh"
MACOS = platform.system() == "Darwin"


@functools.cache
def tool_places() -> tuple[str, ...]:
    """Where macOS keeps what `/usr/bin/python3`, `cc`, `make` and `git` really are. Those
    four are stubs: each reads the link in `/var/select` and runs the program of that name in
    the developer directory, and one that cannot read either only prints `xcode-select`
    errors. `/bin/sh` reads `/var/select` too, and `curl` its configuration in `/etc/ssl`."""
    places = [
        "/private/var/select",
        "/Library/Developer/CommandLineTools",
        "/private/etc/ssl",
        "/private/etc/localtime",
        "/private/var/db/timezone",
    ]
    try:
        chosen = subprocess.run(
            ["/usr/bin/xcode-select", "-p"], capture_output=True, text=True, timeout=10, check=False
        ).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        chosen = ""
    # Xcode in place of the command line tools: the whole application, which the tools in
    # its developer directory reach back into.
    application = re.match(r"(/.+?\.app)(/|$)", chosen)
    if application:
        places.append(application.group(1))
    elif chosen.startswith("/") and chosen not in places:
        places.append(chosen)
    return tuple(places)


def srt_env(scratch: Path) -> dict:
    """What a sandboxed shell is told: a home and a temporary directory of the trial's own,
    since the person's are behind the wall (`git` stops at a `~/.gitconfig` it may not read,
    a compiler at a temporary file it may not write). SRT sets `TMPDIR` itself, from
    `CLAUDE_CODE_TMPDIR`, so that is said too."""
    home, temporary = scratch / "home", scratch / "tmp"
    for made in (home, temporary):
        made.mkdir(parents=True, exist_ok=True)
    return {"HOME": str(home), "TMPDIR": str(temporary), "CLAUDE_CODE_TMPDIR": str(temporary)}


def srt_prefix(
    srt: str, settings: Path, workspace: Path, *read_only: Path, scratch: Path | None = None
) -> list[str]:
    """Constrain tool processes, leaving model connections in the trusted CLI.

    The wall is for the exam: nothing of the person's files, the checks or another trial is
    read. What the tasks have the agent use stays in reach: the system's tools and macOS's
    developer tools to read, the workspace and `scratch` to write, and loopback, which a
    task that starts a server and fetches from it needs (so a service of the person's on
    loopback can be reached too; nothing further out can)."""
    def literal(path: Path) -> dict:
        return {"path": str(path), "literal": True}

    state = literal(workspace / ".corduroy")
    runtime = ["/bin", "/usr/bin", "/usr/lib", "/usr/libexec", "/usr/share", "/System/Library",
               "/dev/null", "/dev/urandom", "/private/etc/passwd", "/private/etc/group"]
    written = [workspace, *([scratch] if scratch else [])]
    settings.write_text(json.dumps({
        "network": {"allowedDomains": [], "deniedDomains": ["*"], "allowLocalBinding": True},
        "filesystem": {
            "denyRead": ["/", state],
            "allowRead": runtime
            + list(tool_places())
            + [literal(path) for path in (*written, *read_only)],
            "allowWrite": [literal(path) for path in written],
            "denyWrite": ["/tmp/claude", state],
        },
    }))
    return [srt, "--settings", str(settings), "--"]


def check_srt(srt: str, path: str) -> None:
    """A successful shell must enter the sandbox, do trial I/O, and be refused a file
    outside the workspace, to read and to write. (What the tasks need there is tried
    afterwards, by `unusable`.)"""
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary).resolve()
        workspace = root / "workspace"
        workspace.mkdir()
        (workspace / "inside").write_text("inside\n")
        outside = root / "outside"
        outside.write_text("outside\n")
        prefix = srt_prefix(srt, root / "settings.json", workspace)
        probe = '''
            IFS= read -r line < inside && test "$line" = inside || exit 10
            printf 'ok\\n' > new && /bin/rm new || exit 11
            if (IFS= read -r line < "$1"); then exit 12; fi
            if (printf 'bad\\n' >> "$1"); then exit 13; fi
            printf 'srt-check-ok\\n'
        '''
        result = subprocess.run(
            [*prefix, "/bin/sh", "-c", probe, "probe", str(outside)],
            cwd=workspace, env={"PATH": path, "HOME": str(Path.home())},
            capture_output=True, text=True, timeout=30, check=False,
        )
        if result.returncode or "srt-check-ok" not in result.stdout.splitlines():
            raise RuntimeError(f"SRT probe failed (exit {result.returncode}): {result.stderr.strip()}")

# What the default agent has beyond the local-model example: searching without a shell, and
# the date. `corduroy init` writes both.
EXTRA_PLUGINS = """
[[plugins]]
name = "search"
path = "search.wasm"
grants = { fs-read = ["{workspace}", "{workspace}/*"] }

[[plugins]]
name = "today"
path = "today.wasm"
grants = { clock = "wall" }
"""


class Config:
    """What one invocation of the bench runs with."""

    def __init__(self, args: argparse.Namespace) -> None:
        self.url: str = args.url
        self.out = Path(args.out).resolve()
        self.timeout: int = args.timeout
        self.workspace_name: str = args.workspace_name
        self.user: str | None = None
        self.uid = self.gid = None
        self.home = None
        self.binary = Path(args.binary).resolve()
        self.plugins = Path(args.plugins).resolve()
        self.model: str = args.label
        # A hosted model in the local one's place: the provider's kind and the model's name.
        self.provider: str | None = args.provider
        self.model_id: str | None = args.model_id
        self.keychain_secret: str | None = args.keychain_secret
        self.key_env: str | None = None if self.keychain_secret else args.api_key_env or (
            {"anthropic": "ANTHROPIC_API_KEY", "openai": "OPENAI_API_KEY"}.get(args.provider or "")
        )
        self.platform = f"{platform.system()} {platform.machine()}"
        self.srt = args.srt_path
        self.srt_path_env = args.srt_path_env
        if self.srt and not self.out.is_relative_to(ROOT / ".bench-runs"):
            raise SystemExit("--srt needs an output directory under .bench-runs in this checkout")
        # macOS: no user to drop to without an administrator's tools, but a sandbox profile
        # that denies the same places, for which nobody has to be root.
        self.sandbox = MACOS and not self.srt and shutil.which("sandbox-exec") is not None
        if os.geteuid() == 0 and not MACOS:
            try:
                entry = pwd.getpwnam(args.user)
            except KeyError:
                subprocess.run(["useradd", "--create-home", args.user], check=True)
                entry = pwd.getpwnam(args.user)
            self.user, self.uid, self.gid, self.home = (
                args.user,
                entry.pw_uid,
                entry.pw_gid,
                entry.pw_dir,
            )

    def stage(self) -> None:
        """Copy the command and its add-ons to where the agent's user can read them, and
        close this directory and the results to it."""
        stage = self.out / "stage"
        (stage / "plugins").mkdir(parents=True, exist_ok=True)
        shutil.copy2(self.binary, stage / "corduroy")
        for wasm in self.plugins.glob("*.wasm"):
            shutil.copy2(wasm, stage / "plugins" / wasm.name)
        shutil.copy2(HERE / "mcp_vault.py", stage / "mcp_vault.py")
        self.binary = stage / "corduroy"
        self.plugins = stage / "plugins"
        self.vault = stage / "mcp_vault.py"
        if self.user:
            os.chmod(HERE, 0o700)
            for path in [self.out, stage, stage / "plugins"]:
                os.chmod(path, 0o755)

    @property
    def isolated(self) -> bool:
        return bool(self.user) or self.sandbox or bool(self.srt)

    def wall(self, *hidden: Path) -> list[str]:
        """What goes before a command on macOS so that it cannot read the checks, the results
        or the `hidden` directories. Empty elsewhere, where `demote` does it."""
        if not self.sandbox:
            return []
        denied = " ".join(
            f'(subpath "{path}")' for path in (HERE, self.out / "private", *hidden)
        )
        profile = f"(version 1)(allow default)(deny file-read* file-write* {denied})"
        return ["sandbox-exec", "-p", profile]

    def hosted(self, text: str) -> str:
        """Agent-file text written for the local model, as it reads for a hosted one: the
        provider keeps its name (`local`), so only its kind and the model's name change."""
        if not self.provider:
            return text
        key = f'\napi_key_env = "{self.key_env}"' if self.key_env else ""
        if self.keychain_secret:
            key = f'\nsecret = "{self.keychain_secret}"'
        text = text.replace(
            f'kind = "openai-compatible"\nbase_url = "{DEFAULT_URL}"',
            f'kind = "{self.provider}"{key}',
        )
        text = text.replace('model = "local" ', f'model = "{self.model_id}" ')
        text = text.replace("[providers.local.models.local]\ncontext_tokens = 16384", "")
        return text.replace("local/local", f"local/{self.model_id}")

    def demote(self) -> dict:
        """Arguments for `subprocess` that run a child as the agent's user."""
        if not self.user:
            return {}
        return {"user": self.uid, "group": self.gid, "extra_groups": []}

    def env(self, extra: dict | None = None, *, provider_key: bool = True) -> dict:
        env = {
            "PATH": self.srt_path_env or os.environ.get("PATH", "/usr/bin:/bin"),
            "HOME": self.home or os.environ.get("HOME", "/"),
            "USER": self.user or os.environ.get("USER", ""),
            "LANG": "C.UTF-8",
            "CORDUROY_PLUGIN_PATH": str(self.plugins),
        }
        # Keychain values are read only by the command, never by this runner. Forward an
        # environment key only when that credential source was selected.
        if provider_key and self.provider and self.key_env and self.key_env in os.environ:
            env[self.key_env] = os.environ[self.key_env]
        env.update(extra or {})
        return env


class Ran:
    """One command that was run: how it ended and what it printed."""

    def __init__(self, code: int | None, out: str, err: str, seconds: float, timed_out: bool):
        self.code, self.out, self.err, self.seconds, self.timed_out = (
            code,
            out,
            err,
            seconds,
            timed_out,
        )


class Trial:
    """One attempt at one task: a fresh workspace, an agent file beside it, and the checks."""

    def __init__(self, config: Config, task: str, number: int) -> None:
        self.config = config
        self.dir = config.out / "trials" / task / str(number)
        if self.dir.exists():
            shutil.rmtree(self.dir)
        self.ws = self.dir / config.workspace_name
        self.private = self.dir / "private"
        self.ws.mkdir(parents=True)
        self.private.mkdir(mode=0o700)
        self.agent = self.dir / "corduroy.toml"
        base = (ROOT / "examples/local-model/corduroy.toml").read_text()
        self.agent_text = config.hosted(base).replace(DEFAULT_URL, config.url) + EXTRA_PLUGINS
        if config.keychain_secret:
            self.agent_text += f'\n[secrets.{config.keychain_secret}]\nkind = "keychain"\n'
        if config.srt:
            fixtures = [config.vault] if task == "mcp" else []
            self.scratch = self.dir / "scratch"
            prefix = srt_prefix(
                config.srt, self.private / "srt.json", self.ws, *fixtures, scratch=self.scratch
            )
            told = ", ".join(
                f"{name} = {json.dumps(value, ensure_ascii=False)}"
                for name, value in srt_env(self.scratch).items()
            )
            self.agent_text = self.agent_text.replace(
                '[kernels.shell]\nkind = "shell"\n',
                f'[kernels.shell]\nkind = "shell"\nprefix = {json.dumps(prefix, ensure_ascii=False)}\n'
                f"env = {{ {told} }}\n",
                1,
            )
        self.failures: list[str] = []
        self.notes: list[str] = []
        self.commands: list[dict] = []
        self._own(self.dir)
        self._own(self.ws)
        if config.user:
            # Models served side by side each have a user of their own (`--jobs`): a trial
            # is its agent's alone, so that none finds a neighbour's work while it is made.
            os.chmod(self.dir, 0o700)

    # -- setting up ---------------------------------------------------------------------

    def _own(self, path: Path) -> None:
        if self.config.user:
            os.chown(path, self.config.uid, self.config.gid)

    def token(self, n: int = 4) -> str:
        """A value nobody could guess, new for this trial."""
        return secrets.token_hex(n)

    def write(self, name: str, content: str, base: Path | None = None) -> Path:
        path = (base or self.ws) / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
        if self.config.user:
            current = path
            while current != (base or self.ws) and current != current.parent:
                self._own(current)
                current = current.parent
        return path

    def read(self, name: str) -> str | None:
        try:
            return (self.ws / name).read_text(errors="replace")
        except OSError:
            return None

    def configure(self, old: str | None = None, new: str = "", append: str = "") -> None:
        """Change the agent file: replace `old` with `new`, or add `append` at its end
        (top-level keys cannot be appended: replace them)."""
        old, new, append = (
            None if old is None else self.config.hosted(old),
            self.config.hosted(new),
            self.config.hosted(append),
        )
        if old is not None:
            if old not in self.agent_text:
                raise RuntimeError(f"the agent file has no {old!r}")
            self.agent_text = self.agent_text.replace(old, new)
        self.agent_text += append

    def unreachable(self) -> None:
        """Point the agent at a model that is not there."""
        nowhere = "http://127.0.0.1:9/v1/chat/completions"
        if self.config.provider:
            kind = f'kind = "{self.config.provider}"'
            self.agent_text = self.agent_text.replace(kind, f'{kind}\nbase_url = "{nowhere}"', 1)
        else:
            self.agent_text = self.agent_text.replace(self.config.url, nowhere)

    def sh(
        self, command: str, cwd: Path | None = None, timeout: int = 60, private: bool = False
    ) -> Ran:
        """A command in the workspace, as the agent's user (for setting up, and for running
        what the model wrote: never as root)."""
        directory = cwd or self.ws
        argv = ["sh", "-c", command]
        env = {}
        if self.config.srt:
            argv = srt_prefix(
                self.config.srt, self.private / "srt-check.json", directory, scratch=self.scratch
            ) + argv
            env = srt_env(self.scratch)
        return self._spawn(argv, directory, None, timeout, env, private, provider_key=not self.config.srt)

    def walled(self, argv: list[str], private: bool = False) -> list[str]:
        """`argv` behind the macOS wall; the trial's own private directory is behind it too
        unless `private` says the command is a check that runs there."""
        return self.config.wall(*([] if private else [self.private])) + argv

    # -- driving ------------------------------------------------------------------------

    def _spawn(self, argv, cwd, stdin, timeout, env, private=False, *, provider_key=True) -> Ran:
        started = time.monotonic()
        process = subprocess.Popen(
            self.walled(argv, private),
            cwd=cwd,
            stdin=subprocess.PIPE if stdin is not None else subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=self.config.env(env, provider_key=provider_key),
            text=True,
            start_new_session=True,
            **self.config.demote(),
        )
        timed_out = False
        try:
            out, err = process.communicate(stdin, timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            os.killpg(process.pid, signal.SIGKILL)
            out, err = process.communicate()
        return Ran(process.returncode, out, err, time.monotonic() - started, timed_out)

    def argv(self, *args: str, session: str = "main") -> list[str]:
        self.agent.write_text(self.agent_text)
        return [
            str(self.config.binary),
            "--agent",
            str(self.agent),
            "--workspace",
            str(self.ws),
            "--session",
            session,
            *args,
        ]

    def corduroy(self, *args, session="main", stdin=None, timeout=None, env=None) -> Ran:
        """Run the command to its end, as another harness would: arguments in, exit code and
        text out."""
        ran = self._spawn(
            self.argv(*args, session=session),
            self.ws,
            stdin,
            timeout or self.config.timeout,
            env or {},
        )
        self.commands.append(
            {
                "args": list(args),
                "session": session,
                "code": ran.code,
                "seconds": round(ran.seconds, 1),
                "timed_out": ran.timed_out,
                "stdout": ran.out[-6000:],
                "stderr": ran.err[-2000:],
            }
        )
        if ran.timed_out:
            self.note(f"`{args[0]}` was killed after {timeout or self.config.timeout}s")
        return ran

    def run(self, prompt: str, **kwargs) -> Ran:
        return self.corduroy("run", prompt, **kwargs)

    def start(self, prompt: str, session: str = "main", env=None) -> subprocess.Popen:
        """Start a run and return at once, for the tasks that interrupt it."""
        return subprocess.Popen(
            self.walled(self.argv("run", prompt, session=session)),
            cwd=self.ws,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=self.config.env(env or {}),
            text=True,
            **self.config.demote(),
        )

    def wait_for(self, predicate, seconds: int = 600) -> bool:
        """Wait until the log satisfies `predicate` (given its events)."""
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if predicate(self.events()):
                return True
            time.sleep(0.5)
        return False

    # -- reading what happened ----------------------------------------------------------

    def events(self, session: str | None = None, topic: str | None = None) -> list[dict]:
        events: list[dict] = []
        try:
            lines = (self.ws / ".corduroy/log.jsonl").read_text(errors="replace").splitlines()
        except OSError:
            return events
        for line in lines:
            try:
                value = json.loads(line)
            except ValueError:
                continue
            if not isinstance(value, list):
                continue
            for event in value:
                if not isinstance(event, dict):
                    continue
                # A payload is the JSON itself (log schema 3). It is text where the store
                # could not hold it so, and in every line an older build wrote.
                payload = event.get("payload")
                if isinstance(payload, str):
                    try:
                        payload = json.loads(payload)
                    except ValueError:
                        payload = {"text": payload}
                event["payload"] = payload if isinstance(payload, dict) else {"value": payload}
                events.append(event)
        return [
            event
            for event in events
            if (session is None or event.get("session") == session)
            and (topic is None or event.get("topic") == topic)
        ]

    def log_text(self) -> str:
        try:
            return (self.ws / ".corduroy/log.jsonl").read_text(errors="replace")
        except OSError:
            return ""

    def answer(self, session: str = "main") -> str:
        """The text of the model's last message in the session."""
        for event in reversed(self.events(session, "message")):
            payload = event.get("payload") or {}
            if payload.get("role") == "assistant":
                return message_text(payload)
        return ""

    def calls(self, tool: str | None = None, session: str | None = None) -> list[dict]:
        """Tool results, each with whether it worked."""
        return [
            result
            for result in tool_results(self.events(session))
            if tool is None or result.get("tool") == tool
        ]

    def used(self, tool: str, session: str | None = None) -> bool:
        return any(call.get("ok") for call in self.calls(tool, session))

    def last_status(self, session: str = "main") -> str | None:
        ends = self.events(session, "turn/end")
        return ends[-1]["payload"].get("status") if ends else None

    def hidden(self, files: dict[str, str], command: str, timeout: int = 60) -> Ran:
        """Run a check the agent never saw, on a copy of its workspace, as its user."""
        copy = self.private / "copy"
        if copy.exists():
            shutil.rmtree(copy)
        shutil.copytree(self.ws, copy, ignore=shutil.ignore_patterns(".corduroy"), symlinks=True)
        for name, content in files.items():
            (copy / name).write_text(content)
        if self.config.user:
            os.chmod(self.private, 0o711)
            subprocess.run(["chown", "-R", f"{self.config.uid}:{self.config.gid}", copy], check=True)
        try:
            return self.sh(command, cwd=copy, timeout=timeout, private=True)
        finally:
            os.chmod(self.private, 0o700)

    # -- judging ------------------------------------------------------------------------

    def expect(self, condition, message: str) -> bool:
        """An outcome the task needs. Every one that does not hold is recorded."""
        if not condition:
            self.failures.append(message)
        return bool(condition)

    def note(self, message: str) -> None:
        """Something worth knowing that does not decide pass or fail."""
        self.notes.append(message)

    def record(self, task: dict, number: int, seconds: float, crashed: str | None) -> dict:
        events = self.events()
        usage = [event["payload"] for event in events if event.get("topic") == "usage"]
        results = tool_results(events)
        gates = [
            event["payload"]
            for event in events
            if event.get("topic") == "gate/decision"
            and event["payload"].get("verdict") != "allow"
        ]
        tools: dict[str, int] = {}
        for result in results:
            tools[result.get("tool", "?")] = tools.get(result.get("tool", "?"), 0) + 1
        return {
            "task": task["name"],
            "tier": task["tier"],
            "trial": number,
            "model": self.config.model,
            "platform": self.config.platform,
            "passed": not self.failures and not crashed,
            "failures": self.failures + ([f"the bench itself failed: {crashed}"] if crashed else []),
            "notes": self.notes,
            "seconds": round(seconds, 1),
            "model_requests": sum(1 for event in events if event.get("topic") == "model/request"),
            "input_tokens": sum(entry.get("input_tokens", 0) for entry in usage),
            "output_tokens": sum(entry.get("output_tokens", 0) for entry in usage),
            "max_input_tokens": max((entry.get("input_tokens", 0) for entry in usage), default=0),
            "stops": sorted({entry.get("stop") or "?" for entry in usage}),
            "tools": tools,
            "tool_errors": [
                {"tool": result.get("tool"), "output": str(result.get("output"))[:300]}
                for result in results
                if not result.get("ok")
            ],
            "gates": [
                {key: gate.get(key) for key in ("tool", "verdict", "gate", "reason")}
                for gate in gates
            ],
            "denied": [
                event["payload"] for event in events if event.get("topic") == "capability/denied"
            ],
            "plugin_trouble": [
                {"topic": event["topic"], "payload": event.get("payload")}
                for event in events
                if event.get("topic")
                in ("plugin/failed", "plugin/trapped", "plugin/stopped", "plugin/cascade")
            ],
            "statuses": [
                event["payload"].get("status")
                + (f": {event['payload']['reason'][:300]}" if event["payload"].get("reason") else "")
                for event in events
                if event.get("topic") == "turn/end"
            ],
            "compactions": sum(1 for event in events if event.get("topic") == "compaction"),
            "sessions": sorted({event["session"] for event in events if event.get("session")}),
            "answer": self.answer()[:1500],
            "commands": self.commands,
        }


def tool_results(events: list[dict]) -> list[dict]:
    """Every tool call's result. A call that waited (`delegate`) gets its `tool/result` with
    the `signal` its `tool/wait` named. A log from before that has the signal alone, which
    says neither which call it answers nor whether it worked: there it is paired by name
    and counted as working."""
    answered = {
        event["payload"].get("id") for event in events if event.get("topic") == "tool/result"
    }
    results = []
    waiting: dict[str, dict] = {}
    for event in events:
        payload = event["payload"]
        if event.get("topic") == "tool/result":
            results.append(payload)
        elif event.get("topic") == "tool/wait":
            waiting[payload.get("signal")] = payload
        elif event.get("topic") == "signal" and payload.get("name") in waiting:
            call = waiting.pop(payload["name"])
            if call.get("id") not in answered:
                results.append(
                    {"id": call.get("id"), "tool": call.get("tool"), "ok": True, "output": payload.get("payload")}
                )
    return results


def message_text(payload: dict) -> str:
    """The text parts of a `message` event's payload."""
    parts = payload.get("parts")
    if isinstance(parts, str):
        return parts
    texts = []
    for part in parts or []:
        if isinstance(part, str):
            texts.append(part)
        elif isinstance(part, dict) and isinstance(part.get("text"), str) and part.get(
            "type", "text"
        ) in ("text", None):
            texts.append(part["text"])
    return "\n".join(texts)


def summarize(path: Path, isolated: bool | None = None) -> str:
    rows: dict[str, list[dict]] = {}
    for line in path.read_text().splitlines():
        record = json.loads(line)
        rows.setdefault(record["task"], []).append(record)
    lines = []
    if isolated is False:
        lines.append("NOT ISOLATED: the agent ran as the bench's own user and could read the checks.\n")
    lines.append("| Task | Tier | Passed | Median s | Requests | Peak input tokens | Why it failed |")
    lines.append("| --- | --- | --- | --- | --- | --- | --- |")
    total = passed_all = 0
    for name, records in rows.items():
        passed = sum(record["passed"] for record in records)
        total += len(records)
        passed_all += passed
        seconds = sorted(record["seconds"] for record in records)
        requests = sorted(record["model_requests"] for record in records)
        reasons: dict[str, int] = {}
        for record in records:
            for failure in record["failures"][:1]:
                # One line, and no bar: it is a cell of a table.
                reason = " ".join(failure.split()).replace("|", "/")[:110]
                reasons[reason] = reasons.get(reason, 0) + 1
        why = "; ".join(f"{count}× {reason}" for reason, count in reasons.items())
        lines.append(
            f"| `{name}` | {records[0]['tier']} | {passed}/{len(records)} | "
            f"{seconds[len(seconds) // 2]:.0f} | {requests[len(requests) // 2]} | "
            f"{max(record['max_input_tokens'] for record in records)} | {why} |"
        )
    lines.append(f"\n{passed_all} of {total} trials passed.")
    return "\n".join(lines)


def read_results(path: Path) -> list[dict]:
    try:
        return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
    except OSError:
        return []


def compare(out: Path, tiers: dict[int, str], ladder: tuple[int, ...]) -> str:
    """The models under `out` side by side: for each tier, the trials each passed, and how
    far up the ladder each got."""
    models = {
        path.parent.parent.name: read_results(path)
        for path in sorted(out.glob("*/private/results.jsonl"))
    }
    models = {name: records for name, records in models.items() if records}
    if not models:
        return f"no results under {out}"
    lines = []
    platforms = sorted({record.get("platform", "?") for rs in models.values() for record in rs})
    lines.append(f"Platform: {', '.join(platforms)}.")
    if any(not record.get("isolated") for records in models.values() for record in records):
        lines.append("NOT ISOLATED in at least one trial: an agent could read the checks.")
    lines.append("")
    lines.append("| Tier | " + " | ".join(models) + " |")
    lines.append("| --- |" + " --- |" * len(models))
    reached: dict[str, int | None] = dict.fromkeys(models)
    for tier in sorted(tiers):
        cells = []
        for name, records in models.items():
            mine = [record for record in records if record["tier"] == tier]
            if not mine:
                cells.append("not tried")
                continue
            passed = sum(record["passed"] for record in mine)
            by_task: dict[str, list[bool]] = {}
            for record in mine:
                by_task.setdefault(record["task"], []).append(record["passed"])
            always = sum(all(results) for results in by_task.values())
            cells.append(f"{passed}/{len(mine)} trials, {always}/{len(by_task)} tasks every time")
            if tier in ladder and passed * 2 >= len(mine):
                reached[name] = tier
        mark = "" if tier in ladder else " (the harness)"
        lines.append(f"| {tier}: {tiers[tier]}{mark} | " + " | ".join(cells) + " |")
    lines.append(
        "| **Highest ladder tier with half its trials passed** | "
        + " | ".join("none" if tier is None else str(tier) for tier in reached.values())
        + " |"
    )
    seconds = {name: sum(record["seconds"] for record in rs) for name, rs in models.items()}
    lines.append(
        "| Hours spent | " + " | ".join(f"{value / 3600:.1f}" for value in seconds.values()) + " |"
    )
    return "\n".join(lines)


def collect(out: Path, tiers: dict[int, str], ladder: tuple[int, ...]) -> str:
    """The comparison, written to `<out>/summary.md`, and every record of every model under
    `out` in one file, `<out>/collected.jsonl`: the file to hand to whoever reads the runs."""
    table = compare(out, tiers, ladder)
    records = [
        record
        for path in sorted(out.glob("*/private/results.jsonl"))
        for record in read_results(path)
    ]
    try:
        (out / "summary.md").write_text(table + "\n")
        with (out / "collected.jsonl").open("w") as file:
            for record in records:
                file.write(json.dumps(record) + "\n")
        table += f"\n\n{len(records)} records collected in {out / 'collected.jsonl'}"
    except OSError as error:
        table += f"\n\nnot collected: {error}"
    return table


def local_model(action: str, model: str, check: bool = True, port: int | None = None) -> str:
    """Run `scripts/local-model.sh` for one of the models it knows by name."""
    env = {**os.environ, "CORDUROY_LOCAL_MODEL": model}
    if port:
        env["CORDUROY_LOCAL_MODEL_PORT"] = str(port)
    done = subprocess.run(
        [str(LOCAL_MODEL), action],
        env=env,
        stdout=subprocess.PIPE,
        # Stopping what may not be running is said to nobody.
        stderr=None if check else subprocess.DEVNULL,
        text=True,
        check=check,
    )
    return done.stdout.strip()


def healthy(url: str) -> bool:
    """Whether the model's server answers (llama-server's `/health`; any answer from another)."""
    base = url.split("/v1/")[0]
    try:
        with urllib.request.urlopen(base + "/health", timeout=5):  # noqa: S310 (loopback)
            return True
    except urllib.error.HTTPError as error:
        return error.code != 503  # 503 is llama-server still loading its model
    except OSError:
        return False


# What the tasks have the agent, and their own checks, do: each must work where the agent
# does. A wall that hides the checks may hide these too (a sandbox that denies macOS's
# developer directory leaves `python3`, `cc`, `make` and `git` as stubs that only fail), and
# then an hour's trials measure the wall and not the model. Each is a small real use, not a
# `--version`: a compiler that starts may still have nowhere to write.
NEEDED = {
    "python3": "python3 -c 'import json, sqlite3, http.server, unittest'",
    "a temporary file": 'f=$(mktemp) && rm -f "$f"',
    "cc": "printf 'int main(void) { return 0; }\\n' > probe.c && cc -o probe probe.c && ./probe",
    "make": "printf 'all:\\n\\t@true\\n' > Makefile && make",
    "git": "git init -q . && git -c user.name=bench -c user.email=bench@example.invalid"
    " commit -q --allow-empty -m probe",
    "curl": "curl --version",
    "a server on loopback": """port=$(python3 -c 'import socket; s = socket.socket(); \
s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])') || exit 1
python3 -m http.server $port --bind 127.0.0.1 >/dev/null 2>&1 & server=$!
for attempt in 1 2 3 4 5 6 7 8 9 10; do
  curl -sf http://127.0.0.1:$port/ >/dev/null && fetched=yes && break; sleep 1
done
kill $server; test "$fetched" = yes""",
}


def unusable(config: Config) -> list[tuple[str, str]]:
    """Each of `NEEDED` that fails behind the wall, with the last thing it said."""
    directory = config.out / "preflight"
    shutil.rmtree(directory, ignore_errors=True)
    workspace = directory / "workspace"
    workspace.mkdir(parents=True)
    wall, env = config.wall(), config.env(provider_key=False)
    if config.user:
        os.chown(workspace, config.uid, config.gid)
    if config.srt:
        scratch = directory / "scratch"
        wall = srt_prefix(config.srt, directory / "srt.json", workspace, scratch=scratch)
        env.update(srt_env(scratch))
    broken = []
    for name, command in NEEDED.items():
        try:
            ran = subprocess.run(
                [*wall, "sh", "-c", command],
                capture_output=True,
                text=True,
                check=False,
                timeout=60,
                cwd=workspace,
                env=env,
                stdin=subprocess.DEVNULL,
                **config.demote(),
            )
        except subprocess.TimeoutExpired:
            broken.append((name, "no answer in 60s"))
            continue
        if ran.returncode != 0:
            said = (ran.stderr.strip() or ran.stdout.strip() or f"exit {ran.returncode}").splitlines()
            broken.append((name, said[-1][:200]))
    shutil.rmtree(directory, ignore_errors=True)
    return broken


def run_model(args, chosen: list[dict], tasks, model: str | None) -> Path:
    """Every chosen task, `--trials` times, against one model; returns the results file.
    With `model`, the bench serves it itself and results go under `<out>/<model>`."""
    port = args.port
    if model:
        args.label = model
        args.out = str(Path(args.base) / model)
        local_model("stop", model, check=False, port=port)
        local_model("start", model, port=port)
        args.url = local_model("url", model, port=port)
    elif args.provider:
        args.label = args.label or args.model_id
        args.out = str(Path(args.out).resolve() / args.label)
    config = Config(args)
    config.out.mkdir(parents=True, exist_ok=True)
    config.stage()
    results = Path(args.results) if args.results else config.out / "private" / "results.jsonl"
    results.parent.mkdir(parents=True, exist_ok=True)
    if config.user and not args.results:
        os.chmod(results.parent, 0o700)
    if config.isolated and not config.srt:
        probe = subprocess.run(
            [*config.wall(), "ls", str(HERE)], capture_output=True, check=False, **config.demote()
        )
        if probe.returncode == 0 and config.user:
            raise SystemExit(f"{config.user} can read {HERE}: the checks are not hidden")
        if probe.returncode == 0:
            sys.stderr.write("sandbox-exec does not hide the checks: running without a wall\n")
            config.sandbox = False
    if not config.isolated:
        sys.stderr.write("no wall: the agent runs as you and could read the checks\n")
    broken = unusable(config)
    if broken:
        raise SystemExit(
            "behind the wall the agent runs in, these do not work, and the tasks need them:\n"
            + "\n".join(f"  {name}: {why}" for name, why in broken)
            + "\nNothing was run. Let the wall reach them (on macOS: /private/var/select and"
            " the developer tools' directory to read, the temporary directory to write,"
            " loopback to connect to; with --srt, `srt_prefix` is where), then run this again."
        )
    if args.fresh and results.exists():
        results.unlink()
    if config.user:
        # A trial that was running when the bench was stopped was never closed: close what
        # is left of it now, before any agent runs.
        for left in (config.out / "trials").glob("*/*"):
            os.chown(left, 0, 0)
            os.chmod(left, 0o700)
    done = {(record["task"], record["trial"]) for record in read_results(results)}

    # Trial by trial, so that a run cut short has tried everything once. Climbing goes tier
    # by tier instead, and stops going up the ladder at the first tier the model mostly fails.
    schedule = [(task, number) for number in range(1, args.trials + 1) for task in chosen]
    if args.climb is not None:
        schedule.sort(key=lambda entry: entry[0]["tier"])
    stopped_at = None
    for task, number in schedule:
        if (task["name"], number) in done:
            continue
        if stopped_at is not None and task["tier"] in tasks.LADDER and task["tier"] > stopped_at:
            continue
        if args.climb is not None and stopped_at is None and task["tier"] in tasks.LADDER:
            below = [
                tier for tier in tasks.LADDER
                if tier < task["tier"] and any(t["tier"] == tier for t in chosen)
            ]
            if below:
                mine = [r for r in read_results(results) if r["tier"] == below[-1]]
                rate = sum(r["passed"] for r in mine) / max(len(mine), 1)
                if rate < args.climb:
                    stopped_at = below[-1]
                    sys.stdout.write(
                        f"{config.model}: {rate:.0%} of tier {stopped_at} passed, under "
                        f"{args.climb:.0%}: not climbing further\n"
                    )
                    continue
        if model and not healthy(config.url):
            sys.stdout.write(f"{model}: the server is not answering; starting it again\n")
            local_model("stop", model, check=False, port=port)
            local_model("start", model, check=False, port=port)
        trial = Trial(config, task["name"], number)
        started = time.monotonic()
        crashed = None
        try:
            task["run"](trial)
        except Exception:  # noqa: BLE001 (a task that breaks is a result, not a crash)
            crashed = traceback.format_exc(limit=4)
        record = trial.record(task, number, time.monotonic() - started, crashed)
        record["isolated"] = config.isolated
        if config.srt:
            record["sandbox_backend"] = "srt-tools"
        if config.user:
            # A later trial must not find this one's attempt.
            os.chown(trial.dir, 0, 0)
            os.chmod(trial.dir, 0o700)
        elif config.sandbox or config.srt:
            kept = config.out / "private" / "trials" / task["name"] / str(number)
            shutil.rmtree(kept, ignore_errors=True)
            kept.parent.mkdir(parents=True, exist_ok=True)
            shutil.move(str(trial.dir), str(kept))
        with results.open("a") as file:
            file.write(json.dumps(record) + "\n")
        mark = "PASS" if record["passed"] else "FAIL"
        sys.stdout.write(
            f"{time.strftime('%H:%M')} {config.model} [{number}/{args.trials}] {mark} "
            f"{task['name']:<26} {record['seconds']:>6.0f}s"
            f" {record['model_requests']:>2} requests  {'; '.join(record['failures'])[:160]}\n"
        )
        sys.stdout.flush()
    if model:
        local_model("stop", model, check=False, port=port)
    sys.stdout.write(f"\n## {config.model}\n\n" + summarize(results, config.isolated) + "\n\n")
    sys.stdout.flush()
    return results


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--trials", type=int, default=3)
    parser.add_argument("--only", help="task names, separated by commas")
    parser.add_argument("--tier", help="tiers to run, separated by commas")
    parser.add_argument("--list", action="store_true", help="list the tasks and stop")
    parser.add_argument("--summary", help="print the table for a results file and stop")
    parser.add_argument("--compare", help="print the models under this directory side by side and stop")
    parser.add_argument(
        "--models",
        help="models `scripts/local-model.sh` knows, separated by commas, or `all`: each is "
        "served in turn and its results go under <out>/<model>",
    )
    parser.add_argument("--label", help="what to call the model in the records, without --models")
    parser.add_argument(
        "--jobs",
        type=int,
        default=1,
        help="how many of --models to run at once, each served on a port of its own",
    )
    parser.add_argument("--port", type=int, help="the port a model of --models is served on (8080)")
    parser.add_argument(
        "--provider",
        help="a hosted model in the local one's place: the provider's kind (`anthropic`, "
        "`openai`), with --model-id; results go under <out>/<label>",
    )
    parser.add_argument("--model-id", help="the hosted model's name, as its provider knows it")
    credentials = parser.add_mutually_exclusive_group()
    credentials.add_argument("--api-key-env", help="the variable holding the key (the kind's own by default)")
    credentials.add_argument(
        "--keychain-secret", metavar="NAME",
        help="read the named Corduroy keychain secret instead of an environment variable",
    )
    parser.add_argument(
        "--climb",
        type=float,
        metavar="RATE",
        help="go tier by tier, and stop going up the ladder after a tier where less than "
        "this share of trials passed (0.25, say): the tiers above are not tried",
    )
    parser.add_argument("--quiet", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--srt", action="store_true", help="sandbox trial tools using SRT on macOS")
    parser.add_argument("--srt-check", action="store_true", help="check SRT isolation without running a model")
    parser.add_argument("--fresh", action="store_true", help="forget earlier results instead of going on from them")
    parser.add_argument("--url", default=os.environ.get("CORDUROY_LOCAL_MODEL_URL", DEFAULT_URL))
    parser.add_argument("--out", default="/var/tmp/corduroy-bench")
    parser.add_argument("--results", help="the results file (default: <out>/private/results.jsonl)")
    parser.add_argument(
        "--workspace-name",
        default="workspace",
        help="what the workspace directory is called (a model may mistake a generic name for no name)",
    )
    parser.add_argument("--timeout", type=int, default=900, help="seconds one command may take")
    parser.add_argument("--user", default="corduroy-bench", help="who the agent runs as, under root")
    parser.add_argument("--binary", default=str(ROOT / "target/release/corduroy"))
    parser.add_argument(
        "--plugins", default=str(ROOT / "plugins/target/wasm32-unknown-unknown/release")
    )
    args = parser.parse_args()
    if args.keychain_secret is not None:
        if not args.provider or not args.model_id:
            parser.error("--keychain-secret needs --provider and --model-id")
        if not re.fullmatch(r"[A-Za-z0-9_-]+", args.keychain_secret):
            parser.error("--keychain-secret must contain only letters, digits, '-' and '_'")
    args.srt_path = args.srt_path_env = None
    if args.srt_check or (args.srt and not (args.summary or args.compare or args.list)):
        srt, node = shutil.which("srt"), shutil.which("node")
        if not MACOS or not srt or not node or not all(Path(p).is_absolute() for p in (srt, node)):
            parser.error("SRT mode needs macOS and absolute PATH entries for srt (0.0.81+) and node")
        try:
            # Resolve version-manager shims in the trusted launcher, before dropping its env.
            executable = subprocess.run(
                [node, "-p", "process.execPath"], check=True, capture_output=True, text=True, timeout=10,
            ).stdout.strip()
            node_path = Path(executable)
            if not node_path.is_absolute() or node_path.name != "node" or not os.access(node_path, os.X_OK):
                raise RuntimeError("node did not resolve to an absolute executable named node")
            args.srt_path = srt
            args.srt_path_env = f"{node_path.parent}:/usr/bin:/bin:/usr/sbin:/sbin"
            check_srt(srt, args.srt_path_env)
        except (OSError, RuntimeError, subprocess.SubprocessError) as error:
            parser.exit(1, f"SRT isolation not verified; no trials started. {error}\n")
        if args.srt_check:
            sys.stdout.write("SRT trial sandbox check passed.\n")
            return 0
        if not Path(args.out).resolve().is_relative_to(ROOT / ".bench-runs"):
            parser.error("--srt needs --out .bench-runs/<run>")
        if Path(args.workspace_name).name != args.workspace_name or args.workspace_name in {"", ".", "..", "private"}:
            parser.error("--srt needs a single, non-reserved trial workspace directory name")
    if args.summary:
        sys.stdout.write(summarize(Path(args.summary)) + "\n")
        return 0

    sys.path.insert(0, str(HERE))
    import tasks  # noqa: PLC0415 (after the path is set)

    if args.compare:
        sys.stdout.write(collect(Path(args.compare), tasks.TIERS, tasks.LADDER) + "\n")
        return 0
    if bool(args.provider) != bool(args.model_id):
        parser.error("--provider and --model-id go together")
    key = args.api_key_env or {"anthropic": "ANTHROPIC_API_KEY", "openai": "OPENAI_API_KEY"}.get(
        args.provider or ""
    )
    if args.provider and not args.keychain_secret and key and not os.environ.get(key) and not args.list:
        parser.error(f"{key} is not set: a hosted model needs its key in the environment")
    if args.provider and args.models:
        parser.error("--provider is one hosted model; run it beside --models, as a command of its own")
    chosen = tasks.TASKS
    if args.only:
        names = args.only.split(",")
        unknown = [name for name in names if name not in {task["name"] for task in chosen}]
        if unknown:
            parser.error(f"no such task: {', '.join(unknown)}")
        chosen = [task for task in chosen if task["name"] in names]
    if args.tier:
        chosen = [task for task in chosen if str(task["tier"]) in args.tier.split(",")]
    if args.list:
        for task in chosen:
            sys.stdout.write(f"{task['tier']}  {task['name']:<26} {task['about']}\n")
        return 0

    if not args.models:
        base = Path(args.out).resolve()
        args.label = args.label or (None if args.provider else "model")
        run_model(args, chosen, tasks, None)
        if args.provider:
            sys.stdout.write(collect(base, tasks.TIERS, tasks.LADDER) + "\n")
        return 0
    known = [line.split()[0] for line in local_model("models", "lfm3-1.7b").splitlines()]
    models = known if args.models == "all" else args.models.split(",")
    unknown = [model for model in models if model not in known]
    if unknown:
        parser.error(f"no such model: {', '.join(unknown)} (known: {', '.join(known)})")
    if args.results:
        parser.error("--results is one file; with --models each model has its own under --out")
    args.base = str(Path(args.out).resolve())
    if args.jobs > 1 and len(models) > 1:
        # Each model is this command again for that model alone, on its own port. The server
        # is built and the files fetched first, one after another: two builds would collide.
        for model in models:
            local_model("setup", model)
        first = args.port or 8080
        if not MACOS:
            # On a processor alone, servers that each take every core starve one another
            # (a twentieth of the speed, measured): share the cores out. A Mac's GPU does
            # the work there, and is shared by itself.
            share = max(1, (os.cpu_count() or 4) // min(args.jobs, len(models)))
            os.environ["CORDUROY_LOCAL_MODEL_ARGS"] = (
                os.environ.get("CORDUROY_LOCAL_MODEL_ARGS", "") + f" --threads {share}"
            ).strip()
        passed = [arg for arg in sys.argv[1:]]
        waiting = list(enumerate(models))
        running: list[subprocess.Popen] = []
        while waiting or running:
            running = [child for child in running if child.poll() is None]
            while waiting and len(running) < args.jobs:
                index, model = waiting.pop(0)
                running.append(
                    subprocess.Popen(
                        [sys.executable, "-u", __file__, *passed, "--models", model,
                         "--port", str(first + index), "--jobs", "1", "--quiet",
                         "--user", f"{args.user}-{index + 1}"]
                    )
                )
            time.sleep(1)
    else:
        for model in models:
            try:
                run_model(args, chosen, tasks, model)
            except (subprocess.CalledProcessError, OSError) as error:
                # One model that cannot be fetched or served must not cost the others their turn.
                sys.stdout.write(f"{model}: not run: {error}\n")
    if not args.quiet:
        sys.stdout.write(collect(Path(args.base), tasks.TIERS, tasks.LADDER) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
