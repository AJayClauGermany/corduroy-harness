# Add an extension kind

An extension is a native plugin: Rust compiled into the command, with the host's authority.
Write one when an add-on cannot do the job: it needs to start processes, open sockets, use a
native library, add a step to the loop or declare a decision point of its own. Otherwise
write an [add-on](../tutorials/first-add-on.md); it needs no rebuild and is sandboxed.

Extensions get into an agent by **kind**. The shipped command has the kinds `shell-kernel`,
`powershell-kernel`, `mcp`, `page`, `env-secret`, `chatgpt-secret` and `http-client`. A new kind means a command of
your own: the shipped command with more kinds, rebuilt. The agent file then turns it on.

## 1. Make a binary crate

```toml
[package]
name = "my-corduroy"
version = "0.1.0"
edition = "2024"

[dependencies]
corduroy-cli = { path = "../corduroy-harness/crates/corduroy-cli" }
corduroy-harness = { path = "../corduroy-harness" }
```

## 2. Add the kind

```rust
use corduroy_cli::command::main_with;
use corduroy_cli::extension::Extensions;
use corduroy_harness::core::plugin_fn;

fn main() -> std::process::ExitCode {
    let extensions = Extensions::builtin().kind("greeter", |asked| {
        asked.only(&["greeting"])?;                      // refuse keys this kind does not know
        let greeting = asked.text("greeting").unwrap_or("hello").to_owned();
        Ok(Box::new(plugin_fn(asked.name, &[], move |ctx| {
            ctx.emit("greeter/ready", format!("{greeting:?}"))?;
            Ok(())
        })))
    });
    main_with(&extensions)
}
```

The closure is called once per entry of that kind. `asked` gives the entry's `name`, its
configuration (`text`, `texts`, `table`, or the raw `config`), and `places` (the workspace,
data directory and the rest, already used to fill in `{workspace}` and friends). It returns
the plugin to load. `plugin_fn(name, inject, apply)` makes one from a closure; anything that
implements `corduroy_core::Plugin` will do.

Everything the plugin registers goes through its `Context`, so it is undone when the plugin
is unloaded: tools, prompt parts, kernels (`Kernels`), HTTP clients, models, secrets,
services (`Services`), decision points (`Gates::declare`), and loop steps
(`corduroy_harness::Operations`). A service the plugin uses in `apply` must be listed in
`inject`, as the README's `audit` example does with `GATES`.

## 3. Turn it on

```toml
[extensions.hello]
kind = "greeter"
greeting = "good morning"
```

```bash
cargo run -- check
```

A misspelt kind is refused with the list of kinds this command has.

## Change more than the kinds

`main_with` is `main_standard(&Standard::new().with_kinds(..))`. Start from
`corduroy_std::Standard` to change the other parts and call `main_standard`:

| `Standard` method | Does |
| --- | --- |
| `with_kinds(&extensions)` | add or replace kinds |
| `without_kind("mcp")` | take a kind away |
| `always("audit")` | load an extension of that kind for every agent, with no configuration |
| `with_log(open)` | keep the log somewhere other than a JSON Lines file |
| `without_addons()` | refuse agent files that list add-ons |

A sandboxed kernel, for example, is a kind registered as `<name>-kernel`; then
`[kernels.x] kind = "<name>"` builds it, and MCP servers started through it are sandboxed
too.

## What an extension may not do

It cannot add a capability. The add-on boundary is the WIT world, and a new capability is a
change to the core. To give add-ons something new, provide a
[service](share-a-service.md) and let add-ons call it with the `service` capability, or
declare a decision point that add-on gates can guard. See
[Three tiers](../design.md#three-tiers-core-extensions-add-ons).
