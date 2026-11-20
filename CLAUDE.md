# corduroy-harness

Minimal agent harness in Rust: a `Model` proposes `Action`s, `Tool`s run them, results are
appended to a `Transcript`, repeat until `Finish` or `Config::max_steps`. Zero dependencies,
synchronous, edition 2024.

## Commands

| Command | What it does |
| --- | --- |
| `cargo t` | Fast tests (`cargo nextest run --all-features`). **Does not run doctests.** |
| `cargo test --doc` | Doctests (nextest skips them) |
| `cargo lint` | Strictest clippy, `-D warnings`. Must be clean before finishing. |
| `cargo fmt --all` | Format. `cargo fmtc` checks without writing. |
| `cargo demo` | Run the demo loop |
| `scripts/check.sh` | fmt + clippy + nextest + doctests + docs: exactly what CI runs |

If `cargo nextest` is missing: `cargo install cargo-nextest --locked` (slow, ~3 min).

## Definition of done

`scripts/check.sh` passes. Fix lint findings; do not silence them.

## Automation (Claude Code hooks)

Configured in `.claude/settings.json`, scripts in `.claude/hooks/`.

- **After every Edit/Write/MultiEdit** of a `*.rs`, `Cargo.toml` or `clippy.toml` file,
  `lint-after-edit.sh` runs `cargo lint` and returns the findings as feedback on that edit. Treat
  them as instructions to fix, not noise. A failure partway through a multi-file change is
  expected; finish the change, then fix what remains.
- **On cloud session start**, `session-start.sh` makes sure clippy, rustfmt and cargo-nextest are
  installed. The first start in a fresh container builds nextest from crates.io (~4 min, the
  prebuilt binaries at `get.nexte.st` are blocked by the egress policy); later starts skip it.

## Lint policy

Clippy runs with `all`, `pedantic`, `nursery`, `cargo` plus a curated set of `restriction`
lints (see `[lints]` in `Cargo.toml`, thresholds in `clippy.toml`). Treat its output as design
guidance, not noise.

- Never add a crate- or module-wide `#![allow(...)]`, and never loosen `[lints]` to get green.
- If one exception is genuinely right, use `#[expect(lint, reason = "...")]` on the narrowest
  item. `allow_attributes` is on, so plain `#[allow]` is flagged; `expect` self-removes when stale.
- `unwrap`, `expect`, `panic!`, `todo!`, indexing and slicing are denied in non-test code. Return
  errors instead. They are permitted inside `#[cfg(test)]` and `tests/`.
- No `println!`/`eprintln!`/`dbg!`. Write to an `io::Write` you were given (see `src/main.rs`).
- `unsafe` is forbidden.
- Public items need docs (`missing_docs`), fallible public fns need `# Errors`.

## Architecture rules

- `Model` and `Tool` are the extension points. Keep the loop in `agent.rs` ignorant of any
  specific model or tool.
- **Tool failures are data, not control flow.** `ToolError` becomes `Entry::ToolError` and goes
  back to the model so it can recover. Only `ModelError` aborts a run.
- Hitting the step limit is a normal `Status::StepLimitReached`, not an `Err`.
- Tests must not need network or an LLM. Drive the loop with `ScriptedModel` or a closure.
- Unit tests live in a `#[cfg(test)] mod tests` beside the code; behaviour through the public API
  goes in `tests/`. nextest kills tests after 15s (`.config/nextest.toml`), so a non-terminating
  loop shows up as a failure rather than a hang.

## Style

- `rustfmt.toml` sets width 100. Run `cargo fmt --all` before committing.
- Prefer `.to_owned()`/`String::from` over `.to_string()` on `&str` (`str_to_string` is on).
- Use `foo.rs` + `foo/` for modules, never `foo/mod.rs` (`mod_module_files` is on).
- Inline format args: `format!("{name}")`, not `format!("{}", name)`.
