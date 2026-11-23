# Embed the harness in a Rust program

The `corduroy` command is one host for the harness. Your own program can be another: put
together a `Harness`, load plugins, and run prompts. This is also how to test an agent
without a model.

## Dependencies

The crates are not on crates.io yet; depend on them by path (or by git):

```toml
[dependencies]
corduroy-harness = { path = "../corduroy-harness" }                       # the loop, the core, the add-on host
corduroy-providers = { path = "../corduroy-harness/crates/corduroy-providers" }  # real models
corduroy-jsonl = { path = "../corduroy-harness/crates/corduroy-jsonl" }         # the log on disk
corduroy-shell = { path = "../corduroy-harness/crates/corduroy-shell" }         # a shell kernel
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## A durable agent with one add-on

```rust
use std::error::Error;
use std::path::Path;
use std::sync::Arc;

use corduroy_harness::core::Scope;
use corduroy_harness::wasm::WasmRuntime;
use corduroy_harness::{Harness, Status};
use corduroy_providers::ProviderSpec;
use corduroy_shell::shell_kernel;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let work = Path::new("/srv/agent-work");

    // The model: any provider the agent file could name, built from the same spec.
    let provider = corduroy_providers::build("main", &ProviderSpec::new("anthropic"))?;

    // The log. Sessions already in the file come back, so a second start resumes.
    let events = Arc::new(corduroy_jsonl::open(work.join(".corduroy/log.jsonl"))?);
    let harness = Harness::resume(provider, events)?
        .with_model("claude-sonnet-5-5")
        .with_system_prompt("You are a careful coding agent.");

    // A kernel, and an add-on granted the workspace and that kernel only.
    harness.plugin(shell_kernel("shell", work));
    let runtime = WasmRuntime::new()?.with_root(work);
    let workspace = Scope::subtree(&work.to_string_lossy());
    harness.plugin(
        runtime
            .load("plugins/target/wasm32-unknown-unknown/release/pi_tools.wasm")?
            .named("pi-tools")
            .grant("fs-read", workspace.clone())
            .grant("fs-write", workspace)
            .grant("kernel", Scope::none().allow("shell")),
    );

    let outcome = harness.run("Tidy up the README").await?;
    if let Status::Waiting { asks, .. } = &outcome.status {
        // A gate asked: answer with `harness.answer(&ask.session, &ask.call, Answer::Allow)`.
        println!("{} question(s) waiting", asks.len());
    }
    println!("{}", outcome.status);
    Ok(())
}
```

What to know:

- `Harness::new(provider)` keeps the log in memory; `Harness::resume(provider, events)` uses
  the one you give it. `Harness::open(events)` has no model at all until a `provider` plugin
  is loaded.
- `harness.plugin(..)` loads any plugin, native or WebAssembly. A WebAssembly plugin that
  imports something not granted does not start; `plugin/failed` on the stream says why.
- `run` uses the `main` session; `run_in(&session, prompt)` another. `continue_run(&session)`
  finishes an interrupted run, `answer(..)` answers a gate, `stop(&session)` stops one.
- Model and tool failures are not `Err`: they end the run as `Status::Failed` or reach the
  model as error results. `Err` means the pipeline broke, for example the log refused a
  write.
- `WasmRuntime::protect(path)` puts a path out of reach of every add-on, as the `corduroy`
  command does for its log and agent file. Do it for yours.
- `harness.events()` is the stream: read `events()` for everything so far, or subscribe.

[`examples/demo.rs`](../../examples/demo.rs) is a complete program (`cargo demo` runs it).

## Test without a model

`ScriptedProvider` answers with the messages you give it, in order, so a test drives the
real loop, real plugins and real gates with no network:

```rust
use corduroy_harness::Harness;
use corduroy_harness::scripted::{ScriptedProvider, call, say};
use serde_json::json;

let model = ScriptedProvider::new([
    call("1", "write", &json!({"path": "notes.txt", "content": "one\n"})),
    say("Wrote notes.txt."),
]);
let harness = Harness::new(Arc::new(model))?;
// load plugins as above, then:
let outcome = harness.run("write a note").await?;
```

Assert on outcomes (files written, `tool/result` events, the final status), not on wording.
The tests under [`tests/`](../../tests) and each crate's `tests/` are written this way.

## Without the standard library

`corduroy-core` and `corduroy-harness` build with `no_std` and `alloc`, on any executor,
down to a microcontroller. Turn off default features and bring the device's own log, kernel
and HTTP client. See [Without the standard library](../design.md#without-the-standard-library).
