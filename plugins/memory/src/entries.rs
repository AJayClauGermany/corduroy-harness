//! The file format of a category: one entry per paragraph, `- text`, with an optional
//! `  tags: a, b` line under it.

/// One remembered thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) text: String,
    pub(crate) tags: Vec<String>,
}

/// `name` if it can name a category: 1 to 64 lowercase letters, digits, `-` or `_`.
pub(crate) fn category_name(name: &str) -> Result<&str, String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if ok {
        Ok(name)
    } else {
        Err(format!(
            "`{name}` cannot name a category: use 1 to 64 lowercase letters, digits, - or _"
        ))
    }
}

/// The entries of a category file. Text that is not in the format becomes entries as it
/// stands, one per paragraph, so a hand-edited file loses nothing.
pub(crate) fn parse(text: &str) -> Vec<Entry> {
    text.split("\n\n")
        .map(str::trim)
        .filter(|paragraph| !paragraph.is_empty())
        .map(|paragraph| {
            let mut lines: Vec<&str> = paragraph.lines().collect();
            let tags = lines
                .last()
                .and_then(|line| line.trim().strip_prefix("tags:"))
                .map(|tags| {
                    tags.split(',')
                        .map(str::trim)
                        .filter(|tag| !tag.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            if !Vec::<String>::is_empty(&tags) {
                lines.pop();
            }
            let joined = lines.join("\n");
            let text = joined
                .strip_prefix("- ")
                .unwrap_or(&joined)
                .trim()
                .to_owned();
            Entry { text, tags }
        })
        .collect()
}

/// The file for `entries`.
pub(crate) fn render(entries: &[Entry]) -> String {
    entries
        .iter()
        .map(|entry| {
            if entry.tags.is_empty() {
                format!("- {}\n", entry.text)
            } else {
                format!("- {}\n  tags: {}\n", entry.text, entry.tags.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_round_trip_with_their_tags() {
        let entries = vec![
            Entry {
                text: "Prefers tabs".into(),
                tags: vec!["style".into()],
            },
            Entry {
                text: "Run tests with cargo t".into(),
                tags: Vec::new(),
            },
        ];
        let text = render(&entries);
        assert_eq!(
            text,
            "- Prefers tabs\n  tags: style\n\n- Run tests with cargo t\n"
        );
        assert_eq!(parse(&text), entries);
    }

    #[test]
    fn a_hand_written_file_is_read_as_paragraphs() {
        let parsed = parse("first note\n\nsecond\nnote\n");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].text, "second\nnote");
    }

    #[test]
    fn category_names_are_plain() {
        assert!(category_name("project-notes").is_ok());
        assert!(category_name("../etc").is_err());
        assert!(category_name("Big").is_err());
        assert!(category_name("").is_err());
    }
}
