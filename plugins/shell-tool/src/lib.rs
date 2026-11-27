//! The `bash` tool's contract, for the add-ons that offer it.
//!
//! A tool is known to the model, the gates and the log by its name, its input and what its
//! result looks like. An add-on that offers `bash` in place of another's has to agree with
//! it on all three, so they are written once, here: [`SCHEMA`] and [`Call`] (the input),
//! [`spec`] (the tool as it is declared), [`run`] (the call into a kernel) and [`shown`]
//! (the result). What an add-on adds of its own is what it hands [`run`] as the script, and
//! how much of the output it keeps ([`Limits`]).
//!
//! This is a library, not an add-on: it builds into the add-ons that use it.

use std::fmt::Write as _;

use corduroy_guest::host::kernel;
use corduroy_guest::{Keep, ToolError, ToolSpec};
use serde::Deserialize;

/// The tool's name.
pub const NAME: &str = "bash";

/// The JSON Schema of the tool's input.
pub const SCHEMA: &str = r#"{"type":"object","properties":{"command":{"type":"string"},"timeout":{"type":"integer","description":"Seconds before the command is stopped (default 120)"}},"required":["command"]}"#;

/// Seconds a command may run when the call does not say.
pub const DEFAULT_TIMEOUT: u32 = 120;

/// The tool's input.
#[derive(Deserialize)]
pub struct Call {
    /// What to run, as the model wrote it.
    pub command: String,
    /// Seconds before the command is stopped.
    pub timeout: Option<u32>,
}

/// How much of a command's output is kept: its last lines, within a number of bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Lines kept at most.
    pub lines: usize,
    /// Bytes kept at most.
    pub bytes: usize,
}

/// The tool as an add-on declares it: `bash`, with `description`, neither replay-safe nor
/// read-only (a command can do anything), and a result of at most `tokens` tokens of which
/// the end is kept, where the error is.
#[must_use]
pub fn spec(description: &str, tokens: u32) -> ToolSpec {
    ToolSpec::new(NAME, description, SCHEMA).result(tokens, Keep::Tail)
}

/// Run `script` in the host kernel `kernel` for a call that asked for `timeout` seconds,
/// and give the result as the model reads it (see [`shown`]).
///
/// # Errors
///
/// The host's refusal or failure, as a [`ToolError`].
pub fn run(
    kernel: &str,
    script: &str,
    timeout: Option<u32>,
    limits: Limits,
) -> Result<String, ToolError> {
    let timeout_ms = timeout.unwrap_or(DEFAULT_TIMEOUT).saturating_mul(1000);
    let cell = kernel::exec(kernel, script, timeout_ms)?;
    Ok(shown(&cell.output, cell.status, cell.truncated, limits))
}

/// A command's result as the model reads it: the end of `output` within `limits`, a line
/// saying so when earlier output was dropped (here, or by the host: `truncated`), and the
/// exit status when it is not 0. Binary output is named by its kind and size.
#[must_use]
pub fn shown(output: &str, status: i32, truncated: bool, limits: Limits) -> String {
    let (mut shown, dropped) = match binary(output) {
        // What a command printed of a binary file is of no use to a model and can fill its
        // context in one call.
        Some(binary) => (
            format!(
                "[The output was {binary}, not text, so it is not shown. Look at such a \
                 file with `file`, `strings` or `xxd | head`.]\n"
            ),
            false,
        ),
        None => tail(output, limits.lines, limits.bytes),
    };
    if dropped || truncated {
        shown.insert_str(0, "[Earlier output was truncated.]\n");
    }
    if status != 0 {
        if !shown.is_empty() && !shown.ends_with('\n') {
            shown.push('\n');
        }
        // Writing to a `String` cannot fail.
        let _ = write!(shown, "[exit status {status}]");
    }
    shown
}

/// The end of `output`: at most `max_lines` lines and `max_bytes` bytes. Also reports
/// whether anything was dropped.
#[must_use]
pub fn tail(output: &str, max_lines: usize, max_bytes: usize) -> (String, bool) {
    let mut kept: Vec<&str> = Vec::new();
    let mut bytes = 0;
    for line in output.split_inclusive('\n').rev() {
        if kept.len() == max_lines || (!kept.is_empty() && bytes + line.len() > max_bytes) {
            break;
        }
        bytes += line.len();
        kept.push(line);
    }
    let dropped = bytes < output.len();
    kept.reverse();
    (kept.concat(), dropped)
}

/// What `text` is, with its size, where it is not text a model can read: the contents of a
/// binary file, read or printed as if it were text. `None` for text.
///
/// Text holds no NUL, and a file that was not UTF-8 arrives with U+FFFD where its bytes
/// could not be read, so either a NUL or many of those says binary.
#[must_use]
pub fn binary(text: &str) -> Option<String> {
    let unreadable = text.chars().filter(|&c| c == '\u{fffd}').count();
    let total = text.chars().count();
    if !text.contains('\0') && (unreadable < 8 || unreadable * 20 < total) {
        return None;
    }
    let kinds = [
        ("\u{7f}ELF", "a compiled program (ELF)"),
        ("MZ", "a compiled program (PE)"),
        ("\u{fffd}PNG", "a PNG image"),
        ("%PDF", "a PDF document"),
        ("PK\u{3}\u{4}", "a zip archive"),
        ("\u{1f}\u{fffd}", "gzip-compressed data"),
    ];
    let kind = kinds
        .iter()
        .find(|(start, _)| text.starts_with(start))
        .map_or("binary data", |(_, kind)| kind);
    Some(format!("{kind}, {} bytes", text.len()))
}

/// `text` as one word of a POSIX shell: in single quotes, with each single quote inside it
/// closed, escaped and opened again.
#[must_use]
pub fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDE: Limits = Limits {
        lines: 2000,
        bytes: 50 * 1024,
    };

    #[test]
    fn binary_content_is_named_by_kind_and_size_and_text_is_left_alone() {
        // A compiled program printed by `cat`, as a kernel hands it over.
        let program = String::from_utf8_lossy(b"\x7fELF\x02\x01\x01\0\0\0\xff\xfe").into_owned();
        assert_eq!(
            binary(&program).unwrap(),
            format!("a compiled program (ELF), {} bytes", program.len())
        );
        let image = String::from_utf8_lossy(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR").into_owned();
        assert!(binary(&image).unwrap().starts_with("a PNG image, "));
        // No NUL, but too much of it could not be read as text.
        let noise = String::from_utf8_lossy(&[0xff; 64]).into_owned();
        assert!(binary(&noise).unwrap().starts_with("binary data, "));
        // Text, and text with the odd unreadable byte in a long output, is text.
        assert_eq!(binary("fn main() {}\n"), None);
        assert_eq!(binary(""), None);
        assert_eq!(binary("naïve café ✓\n"), None);
        let mostly = format!("{}\u{fffd}\u{fffd}", "a line of a log\n".repeat(50));
        assert_eq!(binary(&mostly), None);
    }

    #[test]
    fn tails_by_line_and_byte() {
        assert_eq!(tail("a\nb\nc\n", 2, 1024), ("b\nc\n".to_owned(), true));
        assert_eq!(tail("a\nb\n", 10, 1024), ("a\nb\n".to_owned(), false));
        assert_eq!(tail("aaaa\nbb\n", 10, 4), ("bb\n".to_owned(), true));
        assert_eq!(tail("", 10, 10), (String::new(), false));
    }

    #[test]
    fn a_result_says_what_was_dropped_and_how_the_command_ended() {
        assert_eq!(shown("ok\n", 0, false, WIDE), "ok\n");
        assert_eq!(shown("no", 2, false, WIDE), "no\n[exit status 2]");
        assert_eq!(shown("", 1, false, WIDE), "[exit status 1]");
        let short = Limits {
            lines: 1,
            bytes: 64,
        };
        assert_eq!(
            shown("a\nb\n", 0, false, short),
            "[Earlier output was truncated.]\nb\n"
        );
        assert_eq!(
            shown("b\n", 0, true, WIDE),
            "[Earlier output was truncated.]\nb\n"
        );
        assert!(shown("\0\0", 0, false, WIDE).starts_with("[The output was binary data, 2 bytes"));
    }

    #[test]
    fn a_word_is_quoted_whatever_it_holds() {
        assert_eq!(quoted("ls -l"), "'ls -l'");
        assert_eq!(quoted("echo 'a b'"), r"'echo '\''a b'\'''");
        assert_eq!(quoted(""), "''");
    }

    #[test]
    fn the_declared_tool_keeps_the_end_of_a_result() {
        let spec = spec("Run a command.", 4096);
        assert_eq!(spec.name, "bash");
        assert_eq!(spec.input_schema, SCHEMA);
        assert!(!spec.replay_safe && !spec.read_only);
        let said: Vec<(&str, &str)> = spec
            .more
            .iter()
            .map(|named| (named.name.as_str(), named.value.as_str()))
            .collect();
        assert_eq!(said, [("result-tokens", "4096"), ("result-keep", "tail")]);
    }
}
