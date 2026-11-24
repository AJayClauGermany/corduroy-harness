# Events

Everything that happens to an agent is an event on one append-only stream, and the stream is
stored in the log (`.corduroy/log.jsonl` for the `corduroy` command) before anyone sees it.
The conversation the model sees is rebuilt from these events alone.

## An event

| Field | Meaning |
| --- | --- |
| `id` | its place in the stream, from 1 |
| `time` | when its commit was stored, in milliseconds since the Unix epoch; never decreases |
| `session` | the session it belongs to; none for events about the harness itself |
| `source` | `user`, `agent` (the model), `environment` (tool results), `host` (the harness), or `plugin:<name>` |
| `cause` | the id of the earlier event it answers, if any |
| `topic` | what kind of event it is (below) |
| `payload` | JSON |

`corduroy log` prints one per line:

```text
2026-11-23T10:15:33Z #53 main [host] gate/decision (after #51) {"point":"tool/call",…}
     time            id  session source topic       cause         payload
```

A step's events are one **commit**: stored together, or not at all.

## The run

Source: the table in [`src/session.rs`](../../src/session.rs).

| Topic | Source | Payload |
| --- | --- | --- |
| `session/start` | host | `{"parent", "task"}`, each only if there is one |
| `message` | user, agent or environment | the message: `{"role", "parts"}` |
| `tool/call` | agent | `{"id", "tool", "input"}`, caused by its `message` |
| `gate/decision` | host | `{"point", "id", "tool", "verdict", "reason", "gate", "caller"}`, caused by its `tool/call` |
| `gate/answer` | user | `{"id", "tool", "allow", "reason"}`: a person's answer to a gate that asked |
| `tool/start` | host | `{"id", "tool"}`: the tool is about to run |
| `tool/result` | environment | `{"id", "tool", "ok", "output"}`, plus `"content"` when the result is not one block of text |
| `tool/wait` | host | `{"id", "tool", "signal"}`: the call's result comes with that signal |
| `signal` | environment | `{"name", "payload"}`: what a waiting call waited for |
| `usage` | agent | `{"model", "input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens"}`, each count only if the provider gave it |
| `compaction` | host | `{"summary", "keep_from", "by"}`: the messages before `keep_from` are summarised |
| `run/restart` | host | `{"attempt"}`: an unfinished run was picked up again |
| `run/stop` | user | `{}`: a person asked the run to stop |
| `turn/end` | host | `{"status"}`: `finished`, `turn-limit`, `failed` (with `"reason"`), `stopped` or `yielded` |

A run is **unfinished** from its first `message` to its `turn/end`. One that has no
`turn/end` because a gate asked, or because a call waits for a signal, is **waiting**, not
interrupted; `corduroy continue` asks the question again rather than counting a restart.

A tool call leaves a trail, one commit at a time: `tool/call`, then `tool/start`, then
`tool/result`. After a crash this says whether each call never started (it runs), was
running (it runs again only if `replay-safe`, else the model gets an `interrupted` error), or
finished (its result is handed over from the log).

## The log itself

These belong to no session.

| Topic | Source | Payload |
| --- | --- | --- |
| `process/start` | host | `{"program", "version", "world", "platform"}`: what the process that opened the log runs as, recorded before anything else it writes; `world` (the plugin world, such as `corduroy:plugin@0.1.0`) only if it hosts add-ons |
| `session/removed` | user | `{"session"}`: a person removed that session from the log (`corduroy sessions remove`), with the sessions it started, each leaving one of these |

## Plugins

| Topic | Source | When |
| --- | --- | --- |
| `plugin/activated` | host | a plugin started |
| `plugin/failed` | host | a plugin could not start (an ungranted import, an error from `activate`, a name already taken) |
| `plugin/disposed` | host | a plugin was unloaded |
| `plugin/requested` | host | an add-on asked to load another (with the component's hash) |
| `plugin/trapped`, `plugin/restarted`, `plugin/stopped` | host | an add-on's instance trapped; it was restarted; it trapped too often and is stopped until reloaded |
| `plugin/cascade` | host | an event was not delivered because it was too many causes deep |
| `plugin/log` | the plugin | a line it wrote with the `log` capability |
| `capability/denied` | host | `{"plugin", "capability", "resource"}`: a call outside a grant |
| `state/set`, `state/remove` | the plugin | a write to its `state` store; the store is rebuilt from these |
| `llm/request`, `llm/response` | the plugin | a completion through `llm` |
| `http/request`, `http/response` | the plugin | a request through `http`; secrets named, never shown; bodies left out |

Add-ons may emit topics of their own through the `events` capability (`skills/changed`,
`skills/review-due`) and subscribe to any topic by glob in their `plugin-info`.

## Compatibility

The log file starts with a header line carrying its schema version (currently 2); older
versions stay readable. Events gain fields and topics; they do not lose or repurpose them.

While a program runs the log only grows. Between runs, removing a session takes its events
out, so a reader must expect gaps in the ids (they are never used again), a `cause` naming
an event that is gone, and, in a log edited by hand, a session whose parent is gone.
Readers should ignore topics and fields they do not know.
