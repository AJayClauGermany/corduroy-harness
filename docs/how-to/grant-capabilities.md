# Grant an add-on what it needs

An add-on can do only what its entry's `grants` allow. This guide covers working out what to
grant, narrowing it, and finding out why an add-on was refused.

## Find out what it needs

Add the entry with no grants and run `check`:

```toml
[[plugins]]
name = "notes-js"
path = "notes_js.wasm"
```

```bash
corduroy check
```

```text
failed    notes-js: plugin `notes-js` was not granted: log, clock, random, state
```

These are the capabilities its component imports. It will not start until each is granted.
For a JavaScript or Python add-on they are exactly the list it was built with
(`--capabilities`); for a Rust add-on, the host functions its code calls.

## Grant each, as narrowly as works

Each capability is scoped by its own kind of resource (see the
[table](../reference/capabilities.md#the-imports)):

```toml
[plugins.grants]
fs-read = ["{workspace}/docs", "{workspace}/docs/*"]   # a directory and everything below it
fs-write = ["{workspace}/docs/*"]                      # files below it, not the directory itself
kernel = "shell"                                       # one kernel, by name
http = ["api.github.com", "uploads.github.com"]        # hosts
llm = "claude-haiku-5-5"                               # a model the provider serves
state = "*"                                            # all of its own keys
log = ["warn", "error"]                                # only some levels
```

- `*` matches any run of characters, `/` included, so `{workspace}/*` is the whole tree below
  the workspace.
- To reach a directory and what is in it, list both `dir` and `dir/*` (listing the directory
  itself needs the first).
- A grant of `"*"` reaches everything the capability covers. Prefer something narrower for
  `fs-*`, `kernel`, `http` and `plugins`.

## Carve out exceptions

Use `allow` and `deny`; a deny always wins:

```toml
fs-write = { allow = ["{workspace}", "{workspace}/*"], deny = ["{workspace}/.git/*", "{workspace}/.env"] }
```

## Link a capability without giving it

An add-on that imports a capability you do not want it to use can be given an empty scope.
It starts, and every call to that capability is refused:

```toml
kernel = []     # pi-tools without a shell: read, write, edit and tree still work
```

## Find out why something was refused

A refused call shows in the terminal as:

```text
  ! line-count was refused `fs-read` on /etc/hosts
```

and is recorded as a `capability/denied` event with the plugin, capability and resource.
To list every refusal so far:

```bash
corduroy log | grep capability/denied
```

The resource is shown as checked: absolute, with symbolic links followed. If a path you
thought was granted is refused, compare it with your pattern; a link out of the workspace is
checked where it points.

## Things a grant cannot open

The agent file, the log (`.corduroy/log.jsonl`) and the compiled-code cache are refused to
`fs-read` and `fs-write` whatever the grant says. An add-on loaded by another add-on gets at
most what its loader holds.
