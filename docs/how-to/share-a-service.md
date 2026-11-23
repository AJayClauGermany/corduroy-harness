# Share a service between plugins

A **service** is how one plugin offers something to others: a name, methods, JSON in and
out, and the caller's name passed along. Native and WebAssembly plugins can both provide and
call services. Use one when an add-on needs something no capability gives (an extension can
offer it as a service), or when several add-ons should share one piece of logic or state.

## Provide one

In Rust, list the service in `activate` and answer in `handle_call`:

```rust
use corduroy_guest::{CallError, Plugin, PluginInfo, ServiceCall};

struct Greeter;

impl Plugin for Greeter {
    fn activate(_config: String) -> Result<PluginInfo, String> {
        Ok(PluginInfo::new("greeter", env!("CARGO_PKG_VERSION")).provide("greetings"))
    }

    fn handle_call(call: ServiceCall) -> Result<String, CallError> {
        match call.method.as_str() {
            "hello" => Ok(format!(r#"{{"greeting":"hello, {}"}}"#, call.caller)),
            other => Err(CallError::Unknown(format!("method `{other}`"))),
        }
    }
}

corduroy_guest::export!(Greeter);
```

In JavaScript, `services: { greetings: { hello(payload, call) { … } } }`; in Python,
`@plugin.service("greetings", "hello")` on a function taking the payload and the call.

The service acts with *its own* grants, not the caller's. Offering a service is granting
some of your power to whoever may call it, so check `call.caller` if it matters who that is.

## Call one

```rust
use corduroy_guest::ToolError;
use corduroy_guest::host::service;

let answer = service::call("greetings", "hello", "{}")
    .map_err(|error| ToolError::Failed(format!("greetings: {error:?}")))?;
```

The caller needs the `service` capability, scoped by service name:

```toml
grants = { service = "greetings" }
```

A call to a service nobody provides fails with `unknown`. A plugin cannot call itself, or
a plugin further up its own call chain; such a call is refused rather than deadlocking.

## The harness's own: `sessions`

The harness provides `sessions`, which starts a child session for a task and makes the
calling tool wait, in the log, for its answer. The `subagents` add-on's `delegate` is built
on it, with nothing but `service = "sessions"` granted.

## From an extension

A native plugin implements `corduroy_core::Service` and registers it in the core's `Services`
registry; WebAssembly callers cannot tell the difference. This is the way to give add-ons a
database, a browser or a queue without adding a capability. See
[Three tiers](../design.md#three-tiers-core-extensions-add-ons) and
[Plugins extending plugins](../design.md#plugins-extending-plugins).
