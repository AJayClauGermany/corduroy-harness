# Add an MCP server

Each `[mcp.<name>]` entry in the agent file connects one
[MCP](https://modelcontextprotocol.io) server, and offers its tools to the model as
`<name>__<tool>`.

## A server you start (stdio)

```toml
[mcp.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "..." }
```

The server is started through a kernel, `shell` by default, so the agent file needs a
`[kernels.shell]` entry (the default agent has one). To start it in another kernel, for
example a sandboxed one, add `kernel = "<name>"`.

## A server you reach over HTTP

```toml
[mcp.docs]
url = "https://mcp.example.com/mcp"
bearer_token_env = "DOCS_MCP_TOKEN"   # the environment variable holding the token
```

## Give it time

```toml
timeout_seconds = 60    # to start, and for each tool call
```

## Check it

```bash
corduroy check
```

```text
tool      github__create_issue
tool      github__search_repositories (read-only)
…
```

A tool the server marks read-only is read-only here too, so the `approval` gate in `smart`
mode lets it through and asks about the rest. To allow or refuse particular ones, name them
in `approval`'s `allow` and `deny` lists by their full `<name>__<tool>` names.

## What to know

- An MCP server is an extension, not an add-on: it runs as a process with whatever the
  kernel that starts it may do, and the capability grants do not apply to it. Only connect
  servers you trust, or start them in a sandboxed kernel.
- If the server cannot be started or reached, the agent runs without its tools, and both
  `check` and every run say so:

  ```text
  ! plugin mcp broken did not start: MCP server `broken`: kernel unavailable: cannot start `definitely-not-a-program`: …
  ```
