# Your first add-on

In this tutorial you will write a tool for the agent in Rust, build it to WebAssembly, plug it
into the agent from [Your first agent](first-agent.md), and watch the sandbox hold: the add-on
will not even start until you grant what it uses, and once it runs it can reach only what you
granted. It takes about fifteen minutes.

You need the checkout, the `corduroy` command on your path and the workspace
`~/corduroy-tutorial` from the first tutorial. The examples assume the checkout is at
`~/corduroy-harness`; change the paths if yours is elsewhere.

## 1. Make a crate

An add-on written in Rust is a library crate compiled for `wasm32-unknown-unknown`. Make one
next to the checkout:

```bash
cd ~ && cargo new --lib line-count && cd line-count
```

```bash
rustup target add wasm32-unknown-unknown
```

Replace `Cargo.toml` with:

```toml
[package]
name = "line-count"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]   # a WebAssembly module, which corduroy turns into a component

[dependencies]
corduroy-guest = { path = "../corduroy-harness/plugins/guest" }
serde = { version = "1", features = ["derive"] }

[profile.release]
opt-level = "s"
lto = true
strip = true
```

`corduroy-guest` is the crate every Rust add-on builds on. It holds the bindings to the
plugin world ([`wit/corduroy.wit`](../../wit/corduroy.wit)) and a `Plugin` trait in which
everything has a sensible default.

## 2. Write the tool

Replace `src/lib.rs` with:

```rust
//! A tool that counts the lines of a file.

use corduroy_guest::host::fs_read;
use corduroy_guest::{Plugin, PluginInfo, ToolError, ToolSpec};
use serde::Deserialize;

/// The tool's input, as the model sends it.
#[derive(Deserialize)]
struct Input {
    path: String,
}

const SCHEMA: &str = r#"{
  "type": "object",
  "properties": { "path": { "type": "string", "description": "The file to count" } },
  "required": ["path"]
}"#;

struct LineCount;

impl Plugin for LineCount {
    fn activate(_config: String) -> Result<PluginInfo, String> {
        let tool = ToolSpec::new("count_lines", "Count the lines of a text file.", SCHEMA)
            .read_only()
            .replay_safe();
        Ok(PluginInfo::new("line-count", env!("CARGO_PKG_VERSION")).tool(tool))
    }

    fn call_tool(name: String, input: String) -> Result<String, ToolError> {
        if name != "count_lines" {
            return Err(ToolError::UnknownTool(name));
        }
        let input: Input = corduroy_guest::parse(&input)?;
        let text = fs_read::read(&input.path)?;
        Ok(format!("{}: {} line(s)", input.path, text.lines().count()))
    }
}

corduroy_guest::export!(LineCount);
```

Three things to notice:

- `activate` says what the add-on contributes: one tool. It is called when the add-on
  starts, with its configuration from the agent file.
- The tool is `read_only` (it changes nothing, so the `approval` gate will not ask you about
  it) and `replay_safe` (calling it twice is the same as once, so after a crash it can simply
  be called again).
- `fs_read::read` is a **host call**. An add-on has no filesystem of its own; it asks the host,
  which checks the path against the add-on's grant first. A refusal comes back as an error,
  and `?` turns it into a tool error the model sees.

## 3. Build it

```bash
cargo build --release --target wasm32-unknown-unknown
```

The add-on is `target/wasm32-unknown-unknown/release/line_count.wasm`, about 70 KB.

## 4. Plug it in

Go to the workspace and add the add-on to the end of `corduroy.toml`:

```bash
cd ~/corduroy-tutorial
```

```toml
[[plugins]]
name = "line-count"
path = "{home}/line-count/target/wasm32-unknown-unknown/release/line_count.wasm"
```

`{home}` is filled in for you, as are `{workspace}` and `{data}`. Now check the agent:

```bash
corduroy check
```

```text
! plugin line-count did not start: plugin `line-count` was not granted: fs-read
…
failed    line-count: plugin `line-count` was not granted: fs-read
```

The add-on calls `fs_read::read`, so its component imports the `fs-read` capability, and
corduroy will not start a plugin that imports something it was not granted. Nobody had to
write a manifest: what an add-on needs is read from the add-on itself.

## 5. Grant it the workspace

Add a `grants` line under the entry:

```toml
[[plugins]]
name = "line-count"
path = "{home}/line-count/target/wasm32-unknown-unknown/release/line_count.wasm"
grants = { fs-read = ["{workspace}", "{workspace}/*"] }
```

```bash
corduroy check
```

```text
…
tool      count_lines (read-only)
…
```

The tool is there. Try it, and ask for something out of reach as well:

```bash
corduroy run "Use count_lines on todo.txt, then on /etc/hosts."
```

```text
▸ count_lines {"path":"todo.txt"}
  ✓ todo.txt: 4 line(s)
▸ count_lines {"path":"/etc/hosts"}
  ! line-count was refused `fs-read` on /etc/hosts
  ✗ permission denied: capability `fs-read` does not cover `/etc/hosts`
todo.txt has 4 lines. I'm not allowed to read /etc/hosts.
```

Neither call asked you anything: the tool is read-only. The second was refused by the host,
not by the add-on, and the refusal is on the log as a `capability/denied` event. The model
was told why, as it is of any tool failure, and carried on.

## 6. Use one more capability

Have the add-on note each file it counts. Change the `use` line and the last line of
`call_tool`:

```rust
use corduroy_guest::host::{fs_read, log::Level};
```

```rust
        let lines = text.lines().count();
        corduroy_guest::log(Level::Info, &format!("counted {}", input.path));
        Ok(format!("{}: {lines} line(s)", input.path))
```

Build it again, and check:

```bash
cd ~/line-count && cargo build --release --target wasm32-unknown-unknown && cd ~/corduroy-tutorial
```

```bash
corduroy check
```

```text
! plugin line-count did not start: plugin `line-count` was not granted: log
```

A new host call is a new import, so a new grant. The `log` capability is scoped by level;
grant it `info`:

```toml
grants = { fs-read = ["{workspace}", "{workspace}/*"], log = "info" }
```

Run it again and the add-on's line shows up under its name:

```text
▸ count_lines {"path":"todo.txt"}
  [plugin:line-count] counted todo.txt
  ✓ todo.txt: 4 line(s)
```

## What you have done

You have written an add-on, seen that what it may do is decided in the agent file and
nowhere else, and that each host function it calls is a capability it must be granted. The
same holds for add-ons written by someone else, or by the agent itself.

Where to go from here:

- [Capabilities](../reference/capabilities.md): the sixteen things an add-on can be granted,
  and the other exports (event handlers, prompt text, gates, services).
- [Write an add-on in JavaScript, TypeScript or Python](../how-to/write-add-ons-in-js-and-python.md).
- [Write a gate](../how-to/write-a-gate.md), or [share a service](../how-to/share-a-service.md).
- The shipped add-ons in [`plugins/`](../../plugins) are complete examples, each a few hundred
  lines; [`plugins/hints`](../../plugins/hints/src/lib.rs) is a good first read.
