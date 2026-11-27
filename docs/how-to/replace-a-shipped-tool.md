# Replace a shipped tool with your own

A tool is known by its name. The model calls `bash`, a gate's rule says `bash`, the log
records `bash`, and none of them knows which add-on answers. So an add-on of yours can stand
in for a shipped one's tool, and everything written against the name keeps working.

The shipped `lean-shell` is the worked example: `bash` in place of `pi-tools`' own, with
each command put through [`rtk`](https://github.com/rtk-ai/rtk), which shortens what a
command prints before the model reads it.

## Take the tool away from the add-on that has it

A name belongs to one add-on. The second to offer a name already taken does not start, and
`corduroy check` shows it as failed (`` `bash` is already registered ``), its other tools
with it. So first say
which of its tools `pi-tools` still offers:

```toml
[[plugins]]
name = "pi-tools"
path = "pi_tools.wasm"
config = { kernel = "shell", tools = ["read", "write", "edit", "tree"] }
[plugins.grants]
fs-read = ["{workspace}", "{workspace}/*"]
fs-write = { allow = ["{workspace}", "{workspace}/*"], deny = ["{workspace}/.git/*"] }
kernel = "shell"
```

`tools` is `pi-tools`' own setting. An add-on of yours that others may want to replace part
of needs one like it; nothing in the harness takes a tool away from an add-on.

## Load the replacement

```toml
[[plugins]]
name = "lean-shell"
path = "lean_shell.wasm"
config = { kernel = "shell" }
grants = { kernel = "shell" }
```

`rtk` has to be installed where the shell finds commands. If it is not, a call fails and
says so; the command is not run without it.

```bash
corduroy check
```

`check` lists the same five tools as before and no add-on under `failed`; with both add-ons
offering `bash` it says `failed    lean-shell: `bash` is already registered`. Nothing
else in the agent file changes: `approval` still asks about `bash` in `smart` mode, and an
`allow` or `deny` list that names it still applies.

To put commands through another program, or the same one another way, set `prefix` and
`run` ([reference](../reference/shipped-add-ons.md#lean-shell)):

```toml
config = { kernel = "shell", prefix = "rtk", run = "{prefix} {command}" }
```

## Write a replacement that really is one

What has to match is what others rely on:

- **The name and the input schema.** The model was told one schema; a gate reads the input.
- **What the tool says of itself**: read-only or not, safe to repeat after a crash or not. A
  gate decides by the first, and the harness by the second when it picks a run up again.
- **The form of the result**, where the model or a later step reads it: for `bash`, the
  exit status line and the line that says output was dropped.

The surest way to match is to share the code. `pi-tools` and `lean-shell` both build on
`plugins/shell-tool`, a library that holds `bash`'s schema, its declaration and the call
into the kernel; each adds only what it does differently. `tests/replacement.rs` runs one
conversation against each and compares the whole trail (the tools offered, each call's
start and result, what the gates were asked and answered), which is the test to copy for a
replacement of your own.

What may differ is what the replacement is for: here, how much a command's output takes of
the model's context, which `lean-shell` declares as a quarter of `pi-tools`'.
