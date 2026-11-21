//! Pure text handling for the tools: paging, tailing and exact replacement.

use serde::Deserialize;

/// One `edit` replacement.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Replacement {
    pub old_text: String,
    pub new_text: String,
}

/// A window onto a file.
pub struct Page {
    pub text: String,
    /// Lines included in `text`.
    pub shown: usize,
    /// Lines in the whole file.
    pub total: usize,
}

/// Up to `limit` lines of `contents` starting at line `offset` (from 1), stopping early
/// rather than exceed `max_bytes`. At least one line is returned if any is in range.
pub fn page(contents: &str, offset: usize, limit: usize, max_bytes: usize) -> Page {
    let total = contents.lines().count();
    let mut text = String::new();
    let mut shown = 0;
    for line in contents.lines().skip(offset - 1).take(limit) {
        if shown > 0 && text.len() + line.len() + 1 > max_bytes {
            break;
        }
        if shown > 0 {
            text.push('\n');
        }
        text.push_str(line);
        shown += 1;
    }
    Page { text, shown, total }
}

/// The end of `output`: at most `max_lines` lines and `max_bytes` bytes. Also reports
/// whether anything was dropped.
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

/// Apply every replacement to `original`. Each `old_text` must occur exactly once in
/// `original` (not in the result of earlier replacements), and no two may overlap.
pub fn replace(original: &str, edits: &[Replacement]) -> Result<String, String> {
    if edits.is_empty() {
        return Err("no edits given".to_owned());
    }
    let mut spans = Vec::with_capacity(edits.len());
    for (index, edit) in edits.iter().enumerate() {
        let number = index + 1;
        if edit.old_text.is_empty() {
            return Err(format!("edit {number}: oldText is empty"));
        }
        let start = match original.matches(edit.old_text.as_str()).count() {
            0 => return Err(format!("edit {number}: oldText was not found")),
            1 => original.find(edit.old_text.as_str()).unwrap_or(0),
            count => {
                return Err(format!(
                    "edit {number}: found {count} occurrences of oldText; it must be unique"
                ));
            }
        };
        spans.push((start, start + edit.old_text.len(), edit.new_text.as_str()));
    }
    spans.sort_by_key(|&(start, _, _)| start);

    let mut result = String::with_capacity(original.len());
    let mut cursor = 0;
    for (start, end, new_text) in spans {
        if start < cursor {
            return Err("edits overlap".to_owned());
        }
        result.push_str(original.get(cursor..start).unwrap_or_default());
        result.push_str(new_text);
        cursor = end;
    }
    result.push_str(original.get(cursor..).unwrap_or_default());
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(old: &str, new: &str) -> Replacement {
        Replacement {
            old_text: old.to_owned(),
            new_text: new.to_owned(),
        }
    }

    #[test]
    fn pages_by_line_and_stops_at_the_byte_limit() {
        let middle = page("a\nb\nc\nd", 2, 2, 1024);
        assert_eq!(
            (middle.text.as_str(), middle.shown, middle.total),
            ("b\nc", 2, 4)
        );

        let capped = page("aaaa\nbbbb\ncccc", 1, 10, 9);
        assert_eq!((capped.text.as_str(), capped.shown), ("aaaa\nbbbb", 2));

        // A single line over the limit is still returned.
        assert_eq!(page("aaaa", 1, 10, 2).shown, 1);
        assert_eq!(page("a", 5, 10, 10).shown, 0);
    }

    #[test]
    fn tails_by_line_and_byte() {
        assert_eq!(tail("a\nb\nc\n", 2, 1024), ("b\nc\n".to_owned(), true));
        assert_eq!(tail("a\nb\n", 10, 1024), ("a\nb\n".to_owned(), false));
        assert_eq!(tail("aaaa\nbb\n", 10, 4), ("bb\n".to_owned(), true));
        assert_eq!(tail("", 10, 10), (String::new(), false));
    }

    #[test]
    fn replaces_against_the_original_text() {
        let edits = [edit("two", "2"), edit("one", "two")];
        assert_eq!(replace("one two", &edits).unwrap(), "two 2");
    }

    #[test]
    fn rejects_missing_ambiguous_empty_and_overlapping_edits() {
        assert_eq!(replace("abc", &[]).unwrap_err(), "no edits given");
        assert_eq!(
            replace("abc", &[edit("x", "y")]).unwrap_err(),
            "edit 1: oldText was not found"
        );
        assert_eq!(
            replace("aa", &[edit("a", "b")]).unwrap_err(),
            "edit 1: found 2 occurrences of oldText; it must be unique"
        );
        assert_eq!(
            replace("abc", &[edit("", "b")]).unwrap_err(),
            "edit 1: oldText is empty"
        );
        assert_eq!(
            replace("abcd", &[edit("abc", "x"), edit("bcd", "y")]).unwrap_err(),
            "edits overlap"
        );
    }
}
