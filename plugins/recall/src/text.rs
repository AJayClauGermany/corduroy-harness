//! The text of a `message` event, and a snippet of it.

use serde_json::Value;

/// The text parts of an oi-kit message, as stored in a `message` event, joined by newlines.
pub(crate) fn of(payload: &str) -> String {
    let Ok(message) = serde_json::from_str::<Value>(payload) else {
        return String::new();
    };
    message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Up to `width` characters of `text` on one line, around the first `word` in it.
pub(crate) fn snippet(text: &str, word: &str, width: usize) -> String {
    let flat: Vec<char> = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .collect();
    let lower: Vec<char> = flat.iter().flat_map(|c| c.to_lowercase()).collect();
    let needle: Vec<char> = word.chars().collect();
    let at = if needle.is_empty() || lower.len() != flat.len() {
        0
    } else {
        lower
            .windows(needle.len())
            .position(|window| window == needle.as_slice())
            .unwrap_or(0)
    };
    let start = at.saturating_sub(width / 3);
    let end = (start + width).min(flat.len());
    let mut out: String = flat.get(start..end).unwrap_or_default().iter().collect();
    if start > 0 {
        out.insert(0, '…');
    }
    if end < flat.len() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_text_of_a_message_is_its_text_parts() {
        let payload = r#"{"role":"user","content":[{"type":"text","text":"fix the build"},{"type":"toolRequest","id":"1"},{"type":"text","text":"please"}]}"#;
        assert_eq!(of(payload), "fix the build\nplease");
        assert_eq!(of("not json"), "");
    }

    #[test]
    fn a_snippet_is_around_the_word() {
        let text = format!("{} needle {}", "a ".repeat(100), "b ".repeat(100));
        let shown = snippet(&text, "needle", 30);
        assert!(shown.contains("needle"));
        assert!(shown.starts_with('…') && shown.ends_with('…'));
        assert_eq!(snippet("short", "", 30), "short");
    }
}
