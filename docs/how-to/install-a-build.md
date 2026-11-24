# Install a ready-made build

Every commit on `main` that passes CI is built for Linux (x86-64 and ARM), macOS (Apple
silicon) and Windows (x86-64) and published as a
[release](https://github.com/AJayClauGermany/corduroy-harness/releases) named after the
commit, `build-<short sha>`. The newest is marked latest. Nothing needs Rust.

Until releases have version numbers, the commit is the version: `corduroy --version` prints
it.

## Linux or macOS

```bash
curl -fsSL https://raw.githubusercontent.com/AJayClauGermany/corduroy-harness/main/scripts/install.sh | sh
```

It downloads the latest build for your machine, checks it against the release's
`SHA256SUMS`, keeps it in `~/.local/share/corduroy` and links `~/.local/bin/corduroy` to it.
Run it again to update. To install one particular build, name its release:

```bash
curl -fsSL https://raw.githubusercontent.com/AJayClauGermany/corduroy-harness/main/scripts/install.sh | CORDUROY_BUILD=build-1a2b3c4 sh
```

`CORDUROY_HOME` and `CORDUROY_BIN` change where it is kept and linked.

## Windows, or by hand

Download `corduroy-<platform>.zip` (Windows) or `.tar.gz` from a release, unpack it anywhere
and run `corduroy` from the unpacked folder, or put that folder on your `PATH`. On macOS, a
file downloaded in a browser is quarantined; clear it with
`xattr -dr com.apple.quarantine <folder>` before the first run.

## What is in a build

```text
corduroy-<platform>/
  corduroy            the command, with every extension compiled in
  plugins/*.wasm      the add-ons it ships with (pi_tools.wasm, approval.wasm, ...)
  COMMIT              the commit it was built from
  LICENSE, README.md
```

The command looks for an add-on's `path` in the agent file's directory, then
`CORDUROY_PLUGIN_PATH`, then the `plugins` directory beside itself (a link to the command is
followed first), so the default agent finds its add-ons wherever the folder is unpacked. Keep
the two together. Add-ons of your own go beside your agent file or on `CORDUROY_PLUGIN_PATH`;
see the [plugin path](../reference/agent-file.md#plugins).

## The add-ons alone

`corduroy-addons.tar.gz` in each release holds only `plugins/*.wasm`. WebAssembly is the same
on every platform, so one archive serves a command you built from source or embedded in your
own program: unpack it and point `CORDUROY_PLUGIN_PATH` at its `plugins` directory.

## Next

Choose a model and write an agent file: [Your first agent](../tutorials/first-agent.md) from
step 2.
