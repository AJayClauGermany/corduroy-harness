//! The one piece of JSON the core needs: writing a string literal.
//!
//! Event payloads are JSON by convention so they can cross the plugin boundary, but the core
//! has no dependencies and never needs to parse them.

use alloc::string::String;
use alloc::string::ToString;
use core::fmt::{self, Write as _};

/// Render `value` as a JSON string literal, quotes included.
#[must_use]
pub fn string(value: &str) -> String {
    Literal(value).to_string()
}

struct Literal<'a>(&'a str);

impl fmt::Display for Literal<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_char('"')?;
        for character in self.0.chars() {
            match character {
                '"' => f.write_str("\\\"")?,
                '\\' => f.write_str("\\\\")?,
                '\n' => f.write_str("\\n")?,
                '\r' => f.write_str("\\r")?,
                '\t' => f.write_str("\\t")?,
                control if control.is_control() => write!(f, "\\u{:04x}", u32::from(control))?,
                other => f.write_char(other)?,
            }
        }
        f.write_char('"')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_quotes_backslashes_and_control_characters() {
        assert_eq!(string("plain"), "\"plain\"");
        assert_eq!(string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(string("line\nbreak\ttab"), "\"line\\nbreak\\ttab\"");
        assert_eq!(string("\u{1}"), "\"\\u0001\"");
        assert_eq!(string("ünï"), "\"ünï\"");
    }
}
