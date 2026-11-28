# corduroy documentation

corduroy is an agent harness in which nearly everything is a plugin: the model, the tools,
the text of the system prompt, the shells the agent works in, and the rules that decide what
it may do. Plugins are either compiled in (**extensions**) or dropped in as WebAssembly files
that can do only what they are granted (**add-ons**). Every step of a run is written to a log
before anyone sees it, so a run survives a crash and can be read back afterwards.

These pages follow [Diátaxis](https://diataxis.fr): four kinds of documentation for four
different needs. Pick the one that matches what you are doing right now.

| You want to… | Read | |
| --- | --- | --- |
| **learn** by doing, from nothing | [Tutorials](#tutorials) | step by step, every command shown, nothing to decide |
| **get a task done** you already understand | [How-to guides](#how-to-guides) | recipes for one goal each |
| **look something up** | [Reference](#reference) | every key, command, capability and event |
| **understand** why it works the way it does | [Explanation](#explanation) | the ideas, the trade-offs, where they came from |

New here? Do the two tutorials in order (about half an hour together), then read
[Concepts](explanation/concepts.md).

## Tutorials

1. [Your first agent](tutorials/first-agent.md): build corduroy, run the default agent on a
   folder, approve what it wants to do, leave a question unanswered and come back to it, and
   read the log.
2. [Your first add-on](tutorials/first-add-on.md): write a tool in Rust, build it to
   WebAssembly, plug it into your agent, and see its grant hold.

## How-to guides

Using the `corduroy` command:

- [Install a ready-made build](how-to/install-a-build.md): the command and its add-ons for
  Linux, macOS or Windows, without Rust.
- [Connect a model](how-to/connect-a-model.md): Anthropic, OpenAI, Ollama, any compatible
  endpoint, a ChatGPT plan, or a small model on your own machine, with its key kept in your
  keychain.
- [Have a second model review the work](how-to/review-with-a-second-model.md): Claude writes,
  GPT reviews, or the other way round.
- [Grant an add-on what it needs](how-to/grant-capabilities.md), and find out why one did not
  start or was refused.
- [Decide what runs without asking](how-to/control-approval.md): permission modes, allow and
  deny lists, unattended runs.
- [Stop, resume and recover runs](how-to/recover-runs.md): Ctrl-C, `continue`, `stop`,
  sessions.
- [Watch a run in a browser, and answer it there](how-to/watch-a-run-in-a-browser.md): the
  log as a page, with what went wrong in one place, and, turned on, the page the agent
  serves while it runs: allow or refuse what a gate asks, type a prompt, stop a run.
- [Report a problem](how-to/report-a-problem.md): `corduroy doctor`, which tries the agent
  part by part, and `corduroy report`, what a run did and how long each part took, in one
  piece to hand over.
- [Replace a shipped tool with your own](how-to/replace-a-shipped-tool.md): `bash` from
  another add-on, and what a drop-in replacement has to keep.
- [Add an MCP server](how-to/add-an-mcp-server.md).
- [Give an add-on a secret](how-to/use-secrets-and-http.md) and let it make HTTP requests.

Extending corduroy:

- [Write an add-on in JavaScript, TypeScript or Python](how-to/write-add-ons-in-js-and-python.md).
- [Write a gate](how-to/write-a-gate.md): a plugin that allows, asks about or refuses tool
  calls.
- [Share a service between plugins](how-to/share-a-service.md).
- [Add an extension kind](how-to/add-an-extension-kind.md): a command of your own with native
  plugins compiled in.
- [Embed the harness in a Rust program](how-to/embed-the-harness.md), and test it without a
  model.

Working on corduroy itself:

- [Develop corduroy](how-to/develop-corduroy.md): build, test, lint, what CI runs and how
  builds are published.

## Reference

- [The `corduroy` command](reference/cli.md): commands, options, chat commands, files, exit
  codes, environment variables.
- [The agent file](reference/agent-file.md): every key of `corduroy.toml`.
- [Capabilities and the plugin world](reference/capabilities.md): what an add-on can import,
  how each is scoped, and what it exports.
- [Shipped add-ons](reference/shipped-add-ons.md): tools, configuration and grants of each.
- [Events](reference/events.md): the topics on the log and what each payload holds.
- [The script host](reference/script-host.md): what a script is handed, the `host` object and
  the plugin object it returns.
- [`wit/corduroy.wit`](../wit/corduroy.wit), the plugin world itself, and the API docs
  (`cargo doc --open`) for the Rust crates.

## Explanation

- [Concepts](explanation/concepts.md): the mental model in one page, with a glossary.
- [Security model](explanation/security-model.md): what is trusted, what is sandboxed, and
  what is not.
- [Where the ideas come from](explanation/inspirations.md): the one mechanism taken from each
  of Cordis, Prime Intellect, Hermes, Pi, OpenHands and Goose, how it works here, and what
  was left behind.
- [Design](design.md): the full design record, the six harnesses each idea comes from, what
  is stable, and what is planned.
- [Guest languages](guest-languages.md): how JavaScript and Python plugins were made to work
  without WASI, with measurements.
- [Open questions before 1.0](open-questions.md): every decision still to make before the
  core is frozen. Questions and directions only; a decided one moves to the page its answer
  belongs on.
- [Extensions through C](c-extensions.md): what a prototype of a C interface for
  extensions found. A record for one open question (G23), promised to nobody.
- [Trials](trials.md): the open questions that can be tried both ways today, with the
  commands for each way and a line for what you found.
- [What a small local model does in the harness](local-model-findings.md): the command
  driven from outside through 75 tasks against the local model, several times each: what
  works, what fails and how often, and what the log does not record.
