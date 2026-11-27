# Replace a shipped tool with your own

A tool is known by its name. The model calls `bash`, a gate's rule says `bash`, the log
records `bash`, and none of them knows which add-on answers. So an add-on of yours can stand
in for a shipped one's tool, and everything written against the name keeps working.

The shipped `lean-shell` is the worked example: `bash` in place of `pi-tools`' own, with
each command put through [`rtk`](https://github.com/rtk-ai/rtk), which shortens what a
command prints before the model reads it.

## Say which tool it replaces

A name belongs to one add-on. The second to offer a name already taken does not start, and
`corduroy check` shows it as failed (`` `bash` is already registered ``), its other tools
with it. An add-on takes a name over only where the agent file says so, with `replaces` on
its own entry, listed after the add-on whose tool it replaces:

```toml
[[plugins]]
name = "lean-shell"
path = "lean_shell.wasm"
replaces = ["bash"]
config = { kernel = "shell" }
grants = { kernel = "shell" }
```

From then on a call of `bash` is a call of `lean-shell`'s. `pi-tools` is not changed and
its entry stays as it was: its `bash` is still registered, beneath the one that stands in
for it, and is the one called again if `lean-shell` is unloaded.

`rtk` has to be installed where the shell finds commands. If it is not, a call fails and
says so; the command is not run without it.

```bash
corduroy check
```

`check` lists the same five tools as before, with `bash` marked
`(replaces another add-on's)`, and no add-on under `failed`.
Nothing else in the agent file changes: `approval` still asks about `bash` in `smart` mode,
and an `allow` or `deny` list that names it still applies.

To put commands through another program, or the same one another way, set `prefix` and
`run` ([reference](../reference/shipped-add-ons.md#lean-shell)):

```toml
config = { kernel = "shell", prefix = "rtk", run = "{prefix} {command}" }
```

## What is checked

A replacement is let to take a name only if it can be taken for the tool it replaces. The
harness compares what the two declare, when the replacement is loaded:

- **The name**, which is what `replaces` lists.
- **The input schema**, which has to be the same JSON, spacing aside. The model was told
  one schema, and a gate reads the input.
- **Whether a call may be made again after a crash** (replay-safe), which the harness goes
  by when it picks a run up.

One that differs does not start, and `check` says how:

```text
failed    my-shell: `bash` cannot be replaced by this one: its input schema is not the same as that of the tool it replaces
```

So does one told to replace a name nothing has registered (list the add-on it replaces
first), or a name it does not itself offer.

What a tool says of itself to the model, whether it only reads, and how much of a result
it keeps are its own and may differ. What it returns is not checked: no tool declares the
shape of its output.

## Leaving a tool out instead

`pi-tools` can also be told which of its tools to offer, so that a name is free for another
add-on with nothing checked:

```toml
config = { kernel = "shell", tools = ["read", "write", "edit", "tree"] }
```

`tools` is `pi-tools`' own setting, and few add-ons have one like it. Use it to drop a tool
you do not want at all; to put another in its place, `replaces` is the way that is checked.

## Write a replacement that really is one

What the harness checks is the least of it. What others rely on is more:

- **The name and the input schema**, which are checked.
- **What the tool says of itself**: safe to repeat after a crash or not, which is checked,
  and read-only or not, which is not, and which a gate decides by.
- **The form of the result**, where the model or a later step reads it: for `bash`, the
  exit status line and the line that says output was dropped. Nothing checks this.

The surest way to match is to share the code. `pi-tools` and `lean-shell` both build on
`plugins/shell-tool`, a library that holds `bash`'s schema, its declaration and the call
into the kernel; each adds only what it does differently. `tests/replacement.rs` runs one
conversation against each and compares the whole trail (each call's start and result, what
the gates were asked and answered), which is the test to copy for a replacement of your
own. The one thing the log shows apart is what the model was told: a request names its
tool declarations by hash (`tools_hash`), and a replacement that words its description
differently has another.

What may differ is what the replacement is for: here, how much a command's output takes of
the model's context, which `lean-shell` declares as a quarter of `pi-tools`'.
