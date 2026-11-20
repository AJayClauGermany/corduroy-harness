# corduroy-harness

A minimal agent harness in Rust. A **model** proposes actions, **tools** execute them, and the
results feed back into a **transcript** until the model finishes or a step limit is hit.

The model is a trait, so the loop is fully testable without an LLM or network: use a
`ScriptedModel` for fixed sequences or any `FnMut(&Transcript) -> Result<Action, ModelError>`
closure for reactive behaviour.

## Quick start

```bash
cargo demo      # run the demo loop (rule-based model, no network)
cargo t         # fast tests via cargo-nextest
cargo lint      # strictest clippy, warnings are errors
scripts/check.sh  # everything CI runs
```

Requires [`cargo-nextest`](https://nexte.st): `cargo install cargo-nextest --locked`.

## Layout

| Path | Purpose |
| --- | --- |
| `src/agent.rs` | The loop: `Agent`, `Config`, `Outcome`, `Status` |
| `src/model.rs` | `Model` trait, `Action`, `ScriptedModel` |
| `src/tool.rs` | `Tool` trait, `ToolRegistry`, `ToolError` |
| `src/builtin.rs` | Example tools: `Echo`, `Add` |
| `src/transcript.rs` | `Transcript` and `Entry` |
| `tests/` | Integration tests (public API only) |

See [`CLAUDE.md`](CLAUDE.md) for contributor and agent guidelines.

## License

[Boost Software License 1.0](LICENSE)
