# Security model

What corduroy protects, from whom, and where the protection stops. In short: **add-ons are
sandboxed, extensions and kernels are not, and gates are policy, not a sandbox.** Configure
an agent knowing which of the three you are relying on.

## Who is trusted

| | Trusted? | Why |
| --- | --- | --- |
| The core | yes | it is the thing enforcing the rules; it is small and changes rarely |
| Extensions | yes | native code compiled in by whoever built the command. Nothing checks what it does |
| Add-ons | no | they can do only what their grants allow, whoever wrote them |
| The model | no | everything it does goes through tools, which are plugins under the same rules, and through the gates |
| Kernels | as much as the user running `corduroy` | a shell kernel runs whatever it is given with your permissions |

Native code is never loaded at run time. Only WebAssembly can be loaded while an agent runs,
so code a third party or the agent wrote is always an add-on.

## Three layers that are often all called "sandboxing"

| Layer | Question | Where in corduroy |
| --- | --- | --- |
| Plugin isolation | What can a plugin's code do? | the **core**: WebAssembly with no WASI, granted interfaces only, scope checks, fuel and memory limits |
| Execution sandbox | What can a command the model runs do? | a **kernel**. The shipped shell kernel is *not* a sandbox; a container or bubblewrap kernel would be, and registers under a name like any other |
| Policy | Should this call happen at all? | **gates**: `approval`, `inspect`, or your own |

The default agent relies on all three in different ways. `pi-tools` can only touch the
workspace through `fs-read` and `fs-write`, which the core enforces. But `bash` runs in the
shell kernel, which can reach anything you can; what keeps `bash` in check is the `approval`
gate asking you first. If you switch `approval` to `auto`, `bash` is unconfined.

## What the core guarantees

- **Every side effect of an add-on is a host call checked against a grant.** An add-on that
  imports an ungranted capability never starts; a call outside its scope is refused and
  logged as `capability/denied`.
- **Paths are resolved before they are checked.** Relative paths are made absolute and
  symbolic links followed, so `{workspace}/link-to-etc/passwd` is checked as `/etc/passwd`.
- **Protected paths stay out of reach.** The host binary, the compiled-code cache, and in the
  `corduroy` command the agent file and the log, are refused to `fs-read` and `fs-write`
  whatever a grant says. An add-on cannot rewrite the record or its own grants.
- **Processes start only through kernels.** No capability starts a process another way, so a
  sandboxed kernel also sandboxes MCP servers started through it.
- **No amplification.** An add-on that loads another grants it at most what it holds itself,
  and every load is asked of the gates at `plugin/load`, with the component's hash.
- **No impersonation.** A name belongs to one plugin at a time, and its state and its place as
  a caller go by name, so a child cannot take over another plugin's identity.
- **Secrets never reach an add-on or the log.** `secret` hands out an opaque handle; the host
  writes the value into an HTTP request and records only the secret's name. Response bodies
  are not recorded, since they may echo it. The HTTP client follows no redirects, since the
  grant was checked against one host.
- **Decisions are made before, and logged.** A gate's verdict is stored before the tool can
  start. Gates fail closed: one that traps, or a point nobody declared, denies.
- **Runaway plugins are bounded.** Fuel per call, memory per instance, a limit on event
  cascades, a bounded wait on nested calls and on outbound requests, and a restart limit for
  an add-on that keeps trapping.

## What it does not guarantee

- **A kernel is not a sandbox.** Granting an add-on `kernel = "shell"` gives it your shell.
  Which kernel an add-on may use *is* the security decision.
- **The shell sees the command's environment.** That is where a provider's API key is, so
  the model can print it with `bash`, and what it prints is in the log.
- **A workspace's `corduroy.toml` is used as soon as you run `corduroy` there.** It grants
  capabilities, names kernels and starts MCP servers. Read the file of a repository someone
  else wrote before you do.
- **Extensions are not checked.** An extension kind you compile in can do anything.
- **The model's own message is logged before any gate sees it.** If the model writes a
  secret into a tool call, a denial does not keep it out of the log.
- **`read-only` is the tool's own claim.** `approval` in `smart` mode trusts it. Where you do
  not trust a plugin's word, use `approve` mode or put its tools on the `deny` list.
- **`inspect` is a heuristic.** It catches common destructive commands, credentials and
  injected instructions; it is not a guarantee.

## Practical advice

- Grant the narrowest scope that works: a subtree, not `*`; one kernel; named hosts.
- Deny what must not change even inside an allowed tree, as the default agent does with
  `{workspace}/.git/*`.
- Keep `approval` at `smart` or stricter whenever an add-on holds a shell kernel, unless the
  workspace is disposable.
- Give an agent that needs no shell an empty kernel grant (`kernel = []` on `pi-tools`): the
  add-on still starts, `bash` is refused, and `tree`, `read`, `write` and `edit` work
  through the filesystem capabilities alone.
- Read `corduroy log` for `capability/denied` and `gate/decision` events when something was
  refused; they say who asked for what.

## What 1.0 will promise

Everything above describes what is built. What 1.0 is to promise is decided, and most of it
is not built yet:

> What the agent can reach is what you allowed, whatever the model is told and whatever a
> workspace holds. Nothing it builds or loads gives it more.

The differences from today that matter most to someone configuring an agent:

- **The default agent's shell is confined**, on Linux and macOS, to the files, hosts and
  environment variables listed for it. Where a shell cannot be confined it does not start
  unless you chose the plain one.
- **A credential reaches a shell only if you say so**, and by default as a placeholder
  whose value is written in only on its way to the hosts it is for.
- **A workspace's agent file is a request** until you accept it, and what you allow is kept
  with your account, not in the project.

[What is safe by default](../design.md#what-is-safe-by-default) has the whole of it: who is
trusted, the six guarantees and the test each is to have, and what is not promised. This
page becomes that list as each part is built.
