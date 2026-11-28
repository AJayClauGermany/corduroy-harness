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
| `payload` | text, which is JSON for every topic on this page (see [The payload](#the-payload)) |

`corduroy log` prints one per line:

```text
2026-11-23T10:15:33.412Z #53 main [host] gate/decision (after #51) {"point":"tool/call",…}
     time                id  session source topic       cause         payload
```

A step's events are one **commit**: stored together, or not at all. In the file a commit is
one line, an array of its events. A line that is `[]` is a commit of nothing, which builds
before this one wrote; it reads as nothing.

### The payload

An event's payload is text. For every topic the core and the loop define, which is every
topic on this page, that text is JSON, by contract: an object, to which fields are added and
from which none is taken away. A reader ignores a field it does not know.

An add-on's own topics (`<plugin>/…`, raised through the `events` capability) may hold any
text. JSON is the convention there too, and what other readers expect, but nothing checks
it.

The file holds a payload as the JSON it is, so a line is decoded once:

```text
{"corduroy-log":3}
[{"id":1,"time":1795368645804,"session":"main","source":"user","cause":null,"topic":"message","payload":{"role":"user","parts":[{"type":"text","text":"hi"}]}}]
```

Where `payload` is a JSON **string**, the string is the payload's text, to be read as the
topic says. That is how the file holds a payload it cannot nest without changing it: text
that is not JSON, JSON written with other spacing or key order than the store's own, and
JSON that is itself a string. It is also how every payload was held before schema version
3, so one rule reads every version: *a string is the text; anything else is the payload*.
Version 1 had no `time`. A log begun by an older version is still read, and the first
commit made to it marks it as version 3.

A store other than the file (`corduroy_core::Log`) keeps the text however it likes; what a
plugin reads through `log-read`, and what `corduroy log` prints, is the text in every case.

## The run

Source: the table in [`src/session.rs`](../../src/session.rs).

| Topic | Source | Payload |
| --- | --- | --- |
| `session/start` | host | `{"parent", "task"}`, each only if there is one; with `"from"`, the id of an event of `parent`, for a session that [branched](../prototypes.md#x8-a-session-that-branches-from-another) from it there (a prototype): it begins with the conversation `parent` held once that event was stored |
| `message` | user, agent or environment | the message: `{"role", "parts"}`; with `"via"`, the plugin a person's message was given through (the page), where it was not typed at the terminal |
| `tool/call` | agent | `{"id", "tool", "input"}`, caused by its `message` |
| `gate/decision` | host | `{"point", "id", "tool", "verdict", "reason", "gate", "caller"}`, caused by its `tool/call`. For the point `session/start` (whose request is a [prototype](../prototypes.md#x5-the-decision-point-before-a-session-starts)) it holds the request in the place of the call, `{"point", "session", "parent", "starter", "task", "verdict", "reason", "gate"}`, and is recorded in the session that asked to start the other. With `"ask"`, an id: an ask put to a person at a point other than a tool call ([a prototype](../prototypes.md#x9-an-ask-that-reaches-a-person-anywhere)), as `{"point", "ask", "request", "verdict", "reason", "gate"}`, where `request` is what the gates were asked, as JSON text. One with the verdict `ask` is the question; a later one with `allow` or `deny` and `"answer"`, the id of the `gate/answer` it follows, says the answer was used |
| `gate/answer` | user | `{"id", "tool", "allow", "reason"}`: a person's answer to a gate that asked; with `"via"`, the plugin it was given through (the page), where it was not typed at the terminal. For an ask at another point, `{"point", "ask", "allow", "reason"}`, caused by the `gate/decision` that asked ([a prototype](../prototypes.md#x9-an-ask-that-reaches-a-person-anywhere)). Only one from `user` is an answer |
| `tool/start` | host | `{"id", "tool"}`: the tool is about to run |
| `tool/result` | environment | `{"id", "tool", "ok", "output", "bytes", "tokens"}`, plus `"content"` when the result is not one block of text, and `"signal"` where the call waited and this is what that signal brought. What the tool returned, whole: the `message` that carries it to the model holds it cut where it was more than the call or the step [may add](../design.md#keeping-within-what-the-model-holds). `bytes` is the size of its text and `tokens` what that is counted as (three bytes to a token; an image is not counted). Where the call had a bound, `"budget"` is the tokens the model is shown of it at most, and `"cut": true` says the result is longer than that; what a step's results may add together can cut it further, which only the `message` shows. A log from before the sizes has none |
| `tool/wait` | host | `{"id", "tool", "signal"}`: the call's result comes with that signal |
| `signal` | environment | `{"name", "payload"}`: what a waiting call waited for. The `tool/result` of each call that waited for it follows in the same commit, with `"signal"` naming it, so every call has a `tool/result`; a log from before that has the `signal` alone |
| `model/retry` | host | `{"provider", "model", "attempt", "kind", "reason", "delay_ms", "by"}`: an attempt at the request failed for `reason` (its `kind` as a failed `turn/end` names them) and the request is made again after `delay_ms`. `attempt` counts the retries of one request from 1, whoever made them. `by` says who retries: `client`, where the HTTP client beneath the provider made the request again by itself (its `reason` is in that client's words, and its `delay_ms` is the wait the endpoint asked for, or else what the client's policy gives, near enough); `provider`, for the provider's own policy, which covers what the client does not retry (an answer that breaks off while it is read); a loop or anything else that asks again names itself. A request is made again by one of them only: what the client gave up on is not tried again above it |
| `model/progress` | host | `{"provider", "model", "tokens", "elapsed_ms"}`, with `"text"`, `"thinking"` and `"calls"` (each `{"id", "tool", "input"}`) where there is some: a checkpoint of an answer the model is still writing. It holds what was written since the checkpoint before; `tokens` is what the attempt has written so far, with `"estimated": true` where that is reckoned from the length of the text (three bytes to a token) and not the provider's count; `elapsed_ms` is the time since the request went out. Written at most every five seconds, counted from the first piece, so a short answer has none. Not part of the conversation |
| `model/partial` | host | the same fields, holding the whole of what the model had written when its request ended without an answer, and `"why"`: `stopped` (a person stopped the run), `time-limit`, or `failed` with the `"kind"` a failed `turn/end` names. Written once, and only if something had been written. Evidence for a reader: it is not part of the conversation, the model is never shown it, and a tool call in it is never run |
| `model/request` | host | `{"provider", "model", "messages", "tools", "estimated_tokens"}`: the model is about to be asked, with that many messages and tools. Its own commit, so its time is when the request went out; the model's `message` (or the `turn/end` of a request that failed) is when the answer came. `estimated_tokens` is what the request is reckoned to take (its messages, system prompt and tool declarations, at three bytes of text to a token), which is the only size a request that fails ever has. `"system_hash"` and `"tools_hash"` name the system prompt and the tool declarations it carried, where it carried any: the `model/shown` event with that `hash` holds them. What the provider adds on the way is there where the provider says: `"max_tokens"` (the limit on the answer as the request carries it), `"temperature"`, `"thinking"` and `"timeout_ms"` (what one attempt may take in all). A request something [shaped](../prototypes.md#x2-shaping-a-request) (a prototype) has `"shaped"`, the names of the steps that changed it in the order they ran, and `"messages_hash"`, the hash of the messages it carried as `model/shown` hashes a text: such a request is not the session's conversation as the log projects it. A log from before these has the first four fields alone |
| `model/shown` | host | `{"hash", "kind"}` with `"text"` for `kind` `system` (a system prompt) or `"tools"` for `kind` `tools` (the tool declarations: each `{"name", "description", "input_schema"}`): something a model was shown, recorded once for a log, the first time a request carries it, in the same commit as that request and before it. It belongs to no session, since any session's request may name it, so removing a session leaves it. `hash` is the 64-bit FNV-1a hash of the text (of the declarations, as the JSON they are recorded as), in sixteen hexadecimal digits: it tells one prompt from another and is no protection against a text made to collide. It holds nothing the provider was not sent |
| `usage` | agent | `{"model", "input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens", "reasoning_tokens", "stop"}`, each count only if the provider gave it; `stop` is why the model stopped (`finished`, `limit`: cut off at the most it may write, `tool_calls` or `refused`), if the provider said |
| `compaction` | host | `{"summary", "keep_from", "by"}`: the messages before `keep_from` are summarised. `keep_from` is a message a step begins with: the person's, where the turn began, or the model's, inside a turn |
| `run/restart` | host | `{"attempt"}`: an unfinished run was picked up again |
| `run/stop` | user | `{}`: a person asked the run to stop; `{"via"}` where they asked through a plugin (the page) |
| `turn/end` | host | `{"status"}`: `finished`, `cut-off` (the model answered, and the answer stopped at the most it may write, so it is not whole), `turn-limit`, `time-limit`, `output-limit` (the model was still working when the run reached its [`max_turns`, `max_seconds` or `max_output_tokens`](agent-file.md#top-level-keys)), `failed` (with `"reason"`, and `"kind"` where the model gave no answer: `context-overflow`, `rate-limit`, `auth`, `timeout`, `unreachable` or `other`; a reader takes a kind it does not know as `other`), `stopped` or `yielded`. A reader takes a status it does not know as a run that is over without an answer. With `"used"`, where a model request of the run answered: `{"requests"}` and each count of a `usage` event that some request gave, added up over the run's own requests (not those of a session it started, nor a model an add-on asked). A [prototype](../prototypes.md#x7-what-a-run-used-on-its-end) |

A run is **unfinished** from its first `message` to its `turn/end`. One that has no
`turn/end` because a gate asked, or because a call waits for a signal, is **waiting**, not
interrupted; `corduroy continue` asks the question again rather than counting a restart.

Times say how long things took, since every commit carries when it was stored: the model
from `model/request` to its `message`, a tool from `tool/start` to `tool/result`, a person
from the `gate/decision` that asked to their `gate/answer`. `corduroy report` works these
out.

A tool call leaves a trail, one commit at a time: `tool/call`, then `tool/start`, then
`tool/result` (for a call that waits: `tool/wait`, and the `tool/result` when its signal
comes). After a crash this says whether each call never started (it runs), was
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
| `plugin/suspended` | host | a plugin was stopped because a service it needs went away; it starts again when the service is back |
| `plugin/dropped` | host | `{"plugin", "reason"}`: a plugin another asked to load was not loaded, because the one that asked was unloaded first |
| `plugin/requested` | host | an add-on asked to load another (with the component's hash) |
| `plugin/trapped`, `plugin/restarted`, `plugin/stopped` | host | an add-on's instance trapped; it was restarted; it trapped too often and is stopped until reloaded |
| `plugin/cascade` | host | an event was not delivered because it was too many causes deep |
| `plugin/missed` | host | `{"plugin", "event"}`, caused by that event: an add-on did not hear the event, because it was in a call and as many events were already kept for it as the host keeps (1024 unless the host says otherwise). An event that arrives while an add-on is in a call is otherwise kept, and heard when the call returns |
| `plugin/log` | the plugin | a line it wrote with the `log` capability |
| `capability/denied` | host | `{"plugin", "capability", "resource"}`, and `"call"`, the id of the tool call the plugin was serving, where it was serving one: a call outside a grant. For `service` the resource is `<service>:<method>` |
| `secret/refused` | host | `{"plugin", "secret", "host"}`, and `"call"` as above: a secret was not written into an add-on's request, because it names the hosts it may be sent to and the request's was not one. A [prototype](../prototypes.md#x1-a-secret-names-the-hosts-it-may-go-to) |
| `state/set`, `state/remove` | the plugin | a write to its `state` store; the store is rebuilt from these |
| `llm/request`, `llm/response` | the plugin | a completion through `llm`: `{"model", "system", "messages"}`, then `{"model", "ok", "output"}` with the counts and `stop` of a `usage` event, as far as the provider gave them. The model is named `<provider>/<model>` |
| `http/request`, `http/response` | the plugin | a request through `http`; secrets named, never shown; bodies left out |

Add-ons may emit topics of their own through the `events` capability (`skills/changed`,
`skills/review-due`) and subscribe to any topic by glob in their `plugin-info`.

## Compatibility

The log file starts with a header line carrying its schema version (currently 3); older
versions stay readable. Events gain fields and topics; they do not lose or repurpose them.

While a program runs the log only grows. Between runs, removing a session takes its events
out, so a reader must expect gaps in the ids (they are never used again), a `cause` naming
an event that is gone, and, in a log edited by hand, a session whose parent is gone.
Readers should ignore topics and fields they do not know.
