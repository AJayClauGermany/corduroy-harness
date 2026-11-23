# Write a gate

A gate is a plugin asked at a decision point before something happens, most often
`tool/call`, before each tool call. It answers **allow**, **ask** (a person decides; the run
waits) or **deny** (the model is told your reason). Where several gates guard a point the
strictest answer wins, and the decision is logged before the call can run.

## The request

At `tool/call` a gate receives JSON:

```json
{"session": "main", "id": "c1", "tool": "bash", "input": {"command": "git push --force"},
 "read-only": false, "caller": "agent"}
```

`caller` is `agent` for the model's calls and `plugin:<name>` for an add-on's. An ask from a
plugin's call becomes a denial, since nobody is waiting to answer it.

## In Rust

```rust
//! Refuse force-pushes, ask about anything that touches `deploy/`.

use corduroy_guest::{Plugin, PluginInfo, Verdict};
use serde::Deserialize;

#[derive(Deserialize)]
struct Call {
    tool: String,
    input: serde_json::Value,
}

struct NoForce;

impl Plugin for NoForce {
    fn activate(_config: String) -> Result<PluginInfo, String> {
        Ok(PluginInfo::new("no-force", env!("CARGO_PKG_VERSION")).guard("tool/call"))
    }

    fn gate(_point: String, request: String) -> Verdict {
        let Ok(call) = serde_json::from_str::<Call>(&request) else {
            return Verdict::Deny("could not read the call".to_owned());
        };
        let input = call.input.to_string();
        if call.tool == "bash" && input.contains("push --force") {
            Verdict::Deny("force-pushing is not allowed here; push normally".to_owned())
        } else if input.contains("deploy/") {
            Verdict::Ask(format!("`{}` touches deploy/", call.tool))
        } else {
            Verdict::Allow
        }
    }
}

corduroy_guest::export!(NoForce);
```

Add `serde_json = "1"` to the crate's dependencies, next to `corduroy-guest` and `serde`
(set up as in [Your first add-on](../tutorials/first-add-on.md)).

## In JavaScript

```js
import { definePlugin } from 'corduroy-plugin';

export default definePlugin({
  name: 'no-force',
  gates: {
    'tool/call': ({ tool, input }) => {
      const text = JSON.stringify(input);
      if (tool === 'bash' && text.includes('push --force')) {
        return { deny: 'force-pushing is not allowed here; push normally' };
      }
      if (text.includes('deploy/')) return { ask: `\`${tool}\` touches deploy/` };
      return 'allow';
    },
  },
});
```

## In Python

```python
import json
from corduroy_plugin import Plugin, allow, ask, deny

plugin = Plugin("no-force", "0.1.0")

@plugin.gate("tool/call")
def no_force(request):
    text = json.dumps(request["input"])
    if request["tool"] == "bash" and "push --force" in text:
        return deny("force-pushing is not allowed here; push normally")
    if "deploy/" in text:
        return ask(f"`{request['tool']}` touches deploy/")
    return allow()
```

## Plug it in

```toml
[[plugins]]
name = "no-force"
path = "no_force.wasm"
```

A gate that only reads its request needs no grants. `corduroy check` lists it under `gates`.

## Rules to know

- **Fail closed.** If your gate cannot read the request, deny. A gate that traps or throws
  denies anyway.
- **A gate makes no tool calls.** An add-on that gates `tool/call` and imports `tools` is
  refused at activation, since it would be asked about its own calls.
- **Only declared points.** `tool/call` and `plugin/load` always exist; others exist only if
  an extension declares them. Gating an undeclared point is refused at activation.
- **Remembering answers** ("always allow this") is up to your gate: subscribe to
  `gate/answer` events and keep what you learn in `state`.
- **No rewriting.** A gate cannot change a call's input. Deny with a reason and the model
  corrects the call itself.
