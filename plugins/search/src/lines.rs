//! The lines of a file that match, with the lines around them, written as `grep` writes
//! them. Pure, so it is tested natively.

use std::fmt::Write as _;

/// Characters of one line shown at most; a longer line is cut and says so.
const MAX_LINE: usize = 300;

/// What one file's search found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// How many lines matched.
    pub matches: usize,
    /// The matching lines as `path:12:text`, and, where context was asked for, the lines
    /// around them as `path-11-text` with `--` between runs that are not next to each other.
    /// Empty when nothing matched.
    pub text: String,
}

/// The lines of `contents` for which `is_match` holds, at most `limit` of them, each with
/// `context` lines before and after, named as lines of `path`.
pub fn scan(
    path: &str,
    contents: &str,
    is_match: impl Fn(&str) -> bool,
    context: usize,
    limit: usize,
) -> Found {
    let lines: Vec<&str> = contents.lines().collect();
    let hits: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| is_match(line))
        .map(|(index, _)| index)
        .take(limit)
        .collect();
    let mut found = Found {
        matches: hits.len(),
        text: String::new(),
    };
    // The last line written, so that overlapping context is written once.
    let mut written: Option<usize> = None;
    for hit in &hits {
        let first = hit.saturating_sub(context);
        let last = hit
            .saturating_add(context)
            .min(lines.len().saturating_sub(1));
        let start = written.map_or(first, |written| first.max(written + 1));
        if context > 0 && written.is_some_and(|written| start > written + 1) {
            found.text.push_str("--\n");
        }
        for (index, line) in lines.iter().enumerate().take(last + 1).skip(start) {
            let mark = if hits.contains(&index) { ':' } else { '-' };
            // Writing to a `String` cannot fail.
            let _ = writeln!(found.text, "{path}{mark}{}{mark}{}", index + 1, cut(line));
            written = Some(index);
        }
    }
    found
}

/// `line`, cut to [`MAX_LINE`] characters.
fn cut(line: &str) -> String {
    let mut shown: String = line.chars().take(MAX_LINE).collect();
    if shown.len() < line.len() {
        shown.push_str(" […]");
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n";

    #[test]
    fn matching_lines_are_named_by_path_and_number() {
        let found = scan("a.txt", FILE, |line| line.contains('v'), 0, 10);
        assert_eq!(found.matches, 2);
        assert_eq!(found.text, "a.txt:5:five\na.txt:7:seven\n");
    }

    #[test]
    fn context_is_written_once_and_runs_apart_are_separated() {
        let found = scan(
            "a",
            FILE,
            |line| ["one", "two", "seven"].contains(&line),
            1,
            10,
        );
        assert_eq!(found.matches, 3);
        assert_eq!(
            found.text,
            "a:1:one\na:2:two\na-3-three\n--\na-6-six\na:7:seven\na-8-eight\n"
        );
    }

    #[test]
    fn the_limit_stops_the_count_and_nothing_found_is_empty() {
        let found = scan("a", FILE, |_| true, 0, 3);
        assert_eq!(found.matches, 3);
        assert_eq!(found.text.lines().count(), 3);
        assert_eq!(scan("a", FILE, |_| false, 2, 3), Found::default());
        assert_eq!(scan("a", "", |_| true, 2, 3), Found::default());
    }

    #[test]
    fn a_long_line_is_cut_on_a_character() {
        let long = "é".repeat(400);
        let found = scan("a", &long, |_| true, 0, 1);
        assert!(found.text.ends_with(" […]\n"), "{}", found.text);
        assert_eq!(found.text.chars().filter(|c| *c == 'é').count(), 300);
    }
}
