#!/usr/bin/env python3
"""Drive `corduroy` from outside, as another harness would, through a list of tasks against a
real (small, local) model, several times each, and write down what happened.

    scripts/local-model.sh start
    cargo build --release -p corduroy-cli && cargo plugins
    sudo python3 scripts/model-bench/bench.py --trials 3            # everything
    sudo python3 scripts/model-bench/bench.py --only read,edit      # some tasks
    python3 scripts/model-bench/bench.py --summary RESULTS.jsonl    # the table again

The tasks are in `tasks.py`. Each one sets a workspace up, runs the command (`run`, `continue`,
`stop`, a signal) and then checks the outcome from outside: files, exit codes, the event log.

**The agent must not be able to read its own exam.** Its shell kernel is not a sandbox, so run
as root this drops the agent to an unprivileged user (`--user`, made if missing) and keeps
this directory, every expected value and every hidden test where that user cannot read them.
Run as anyone else there is no such wall, and the summary says so.

Nothing here is a test of the repository (`cargo t` never runs it): it needs a model and
takes hours. Standard library only.
"""

from __future__ import annotations

import argparse
import json
import os
import pwd
import re
import secrets
import shutil
import signal
import subprocess
import sys
import time
import traceback
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
DEFAULT_URL = "http://127.0.0.1:8080/v1/chat/completions"

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
        if os.geteuid() == 0:
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

    def demote(self) -> dict:
        """Arguments for `subprocess` that run a child as the agent's user."""
        if not self.user:
            return {}
        return {"user": self.uid, "group": self.gid, "extra_groups": []}

    def env(self, extra: dict | None = None) -> dict:
        env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "HOME": self.home or os.environ.get("HOME", "/"),
            "USER": self.user or os.environ.get("USER", ""),
            "LANG": "C.UTF-8",
            "CORDUROY_PLUGIN_PATH": str(self.plugins),
        }
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
        self.agent_text = base.replace(DEFAULT_URL, config.url) + EXTRA_PLUGINS
        self.failures: list[str] = []
        self.notes: list[str] = []
        self.commands: list[dict] = []
        self._own(self.dir)
        self._own(self.ws)

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
        if old is not None:
            if old not in self.agent_text:
                raise RuntimeError(f"the agent file has no {old!r}")
            self.agent_text = self.agent_text.replace(old, new)
        self.agent_text += append

    def sh(self, command: str, cwd: Path | None = None, timeout: int = 60) -> Ran:
        """A command in the workspace, as the agent's user (for setting up, and for running
        what the model wrote: never as root)."""
        return self._spawn(["sh", "-c", command], cwd or self.ws, None, timeout, {})

    # -- driving ------------------------------------------------------------------------

    def _spawn(self, argv, cwd, stdin, timeout, env) -> Ran:
        started = time.monotonic()
        process = subprocess.Popen(
            argv,
            cwd=cwd,
            stdin=subprocess.PIPE if stdin is not None else subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=self.config.env(env),
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
            self.argv("run", prompt, session=session),
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
                # A payload is JSON kept as text.
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
            return self.sh(command, cwd=copy, timeout=timeout)
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
    """Every tool call's result. A call that waited (`delegate`) has no `tool/result`: its
    result is the `signal` its `tool/wait` named, which says neither which call it answers
    nor whether it worked, so it is paired by name and counted as working."""
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
                reasons[failure[:110]] = reasons.get(failure[:110], 0) + 1
        why = "; ".join(f"{count}× {reason}" for reason, count in reasons.items())
        lines.append(
            f"| `{name}` | {records[0]['tier']} | {passed}/{len(records)} | "
            f"{seconds[len(seconds) // 2]:.0f} | {requests[len(requests) // 2]} | "
            f"{max(record['max_input_tokens'] for record in records)} | {why} |"
        )
    lines.append(f"\n{passed_all} of {total} trials passed.")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--trials", type=int, default=3)
    parser.add_argument("--only", help="task names, separated by commas")
    parser.add_argument("--tier", help="tiers to run, separated by commas")
    parser.add_argument("--list", action="store_true", help="list the tasks and stop")
    parser.add_argument("--summary", help="print the table for a results file and stop")
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
    if args.summary:
        sys.stdout.write(summarize(Path(args.summary)) + "\n")
        return 0

    sys.path.insert(0, str(HERE))
    import tasks  # noqa: PLC0415 (after the path is set)

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

    config = Config(args)
    config.out.mkdir(parents=True, exist_ok=True)
    config.stage()
    results = Path(args.results) if args.results else config.out / "private" / "results.jsonl"
    results.parent.mkdir(parents=True, exist_ok=True)
    if config.user and not args.results:
        os.chmod(results.parent, 0o700)
    if config.user:
        probe = subprocess.run(
            ["ls", str(HERE)], capture_output=True, check=False, **config.demote()
        )
        if probe.returncode == 0:
            sys.stderr.write(f"{config.user} can read {HERE}: the checks are not hidden\n")
            return 1
    else:
        sys.stderr.write("not root: the agent runs as you and could read the checks\n")

    for number in range(1, args.trials + 1):
        for task in chosen:
            trial = Trial(config, task["name"], number)
            started = time.monotonic()
            crashed = None
            try:
                task["run"](trial)
            except Exception:  # noqa: BLE001 (a task that breaks is a result, not a crash)
                crashed = traceback.format_exc(limit=4)
            record = trial.record(task, number, time.monotonic() - started, crashed)
            record["isolated"] = bool(config.user)
            if config.user:
                # A later trial must not find this one's attempt.
                os.chown(trial.dir, 0, 0)
                os.chmod(trial.dir, 0o700)
            with results.open("a") as file:
                file.write(json.dumps(record) + "\n")
            mark = "PASS" if record["passed"] else "FAIL"
            sys.stdout.write(
                f"[{number}/{args.trials}] {mark} {task['name']:<26} {record['seconds']:>6.0f}s"
                f" {record['model_requests']:>2} requests  {'; '.join(record['failures'])[:160]}\n"
            )
            sys.stdout.flush()
    sys.stdout.write("\n" + summarize(results, bool(config.user)) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
