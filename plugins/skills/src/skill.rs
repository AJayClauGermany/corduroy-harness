//! The skill file format: validation, front matter and patching. No host calls.

/// Longest allowed skill name, in bytes.
const MAX_NAME: usize = 64;
/// Longest allowed description, in bytes.
const MAX_DESCRIPTION: usize = 1024;
/// Longest allowed `SKILL.md`, in bytes.
const MAX_CONTENT: usize = 100_000;

/// The parts of a skill's front matter the plugin uses.
#[derive(Debug, PartialEq, Eq)]
pub struct FrontMatter {
    pub name: String,
    pub description: String,
}

/// Check that `name` is safe to use as a directory name: it starts with a lowercase letter
/// or digit and continues with lowercase letters, digits, `.`, `_` or `-`.
pub fn check_name(name: &str) -> Result<(), String> {
    let first_ok = name
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit());
    let rest_ok = name.chars().all(|character| {
        character.is_ascii_lowercase() || character.is_ascii_digit() || "._-".contains(character)
    });
    if first_ok && rest_ok && name.len() <= MAX_NAME && !name.contains("..") {
        Ok(())
    } else {
        Err(format!(
            "`{name}` is not a valid skill name: use up to {MAX_NAME} lowercase letters, \
             digits, dots, dashes and underscores, starting with a letter or digit"
        ))
    }
}

/// Read the `name` and `description` out of a skill's front matter.
pub fn front_matter(content: &str) -> Result<FrontMatter, String> {
    let body = content
        .strip_prefix("---\n")
        .ok_or("a skill must start with a `---` front matter line")?;
    let (header, _) = body
        .split_once("\n---")
        .ok_or("the front matter is not closed with a `---` line")?;
    let field = |key: &str| {
        header
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
            .map(|value| value.trim().trim_matches('"').to_owned())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("the front matter needs a `{key}`"))
    };
    Ok(FrontMatter {
        name: field("name")?,
        description: field("description")?,
    })
}

/// Check that `content` is a well-formed skill called `name`.
pub fn check_content(name: &str, content: &str) -> Result<(), String> {
    if content.len() > MAX_CONTENT {
        return Err(format!("a skill may be at most {MAX_CONTENT} bytes"));
    }
    let meta = front_matter(content)?;
    if meta.name != name {
        return Err(format!(
            "the front matter names the skill `{}`, not `{name}`",
            meta.name
        ));
    }
    if meta.description.len() > MAX_DESCRIPTION {
        return Err(format!(
            "the description may be at most {MAX_DESCRIPTION} bytes"
        ));
    }
    Ok(())
}

/// Replace the single occurrence of `old` in `current` with `new`.
pub fn patch(current: &str, old: &str, new: &str) -> Result<String, String> {
    if old.is_empty() {
        return Err("old_string is empty".to_owned());
    }
    match current.matches(old).count() {
        0 => Err("old_string was not found in the skill".to_owned()),
        1 => Ok(current.replacen(old, new, 1)),
        count => Err(format!(
            "old_string occurs {count} times in the skill; it must be unique"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKILL: &str = "---\nname: release\ndescription: \"How to cut a release\"\n---\n# Steps\n";

    #[test]
    fn names_are_safe_directory_names() {
        assert!(check_name("release-notes_v2.1").is_ok());
        assert!(check_name("").is_err());
        assert!(check_name("../etc").is_err());
        assert!(check_name("a..b").is_err());
        assert!(check_name("Upper").is_err());
        assert!(check_name("-leading").is_err());
        assert!(check_name(&"a".repeat(65)).is_err());
    }

    #[test]
    fn front_matter_gives_name_and_description() {
        assert_eq!(
            front_matter(SKILL).unwrap(),
            FrontMatter {
                name: "release".to_owned(),
                description: "How to cut a release".to_owned(),
            }
        );
        assert!(front_matter("# no front matter").is_err());
        assert!(front_matter("---\nname: x\n").is_err());
        assert!(front_matter("---\nname: x\n---\n").is_err());
    }

    #[test]
    fn content_must_match_its_name_and_limits() {
        assert!(check_content("release", SKILL).is_ok());
        assert!(check_content("other", SKILL).is_err());
        assert!(check_content("release", &format!("{SKILL}{}", "x".repeat(100_000))).is_err());
    }

    #[test]
    fn patch_replaces_exactly_one_occurrence() {
        assert_eq!(patch("a b", "b", "c").unwrap(), "a c");
        assert!(patch("a a", "a", "c").is_err());
        assert!(patch("a", "z", "c").is_err());
        assert!(patch("a", "", "c").is_err());
    }
}
