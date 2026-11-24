//! What the reviewer is asked: the checks on what goes into a command, and the request built
//! from the changes. Pure, so it is tested natively.

use std::fmt::Write as _;

/// The most untracked files listed for the reviewer.
pub const MAX_UNTRACKED: usize = 50;

/// `reference` if it is safe to put, quoted, into a `git diff` command: a branch, tag,
/// commit or expression such as `HEAD~2` or `main@{1}`, never an option.
pub fn reference(reference: &str) -> Result<&str, String> {
    let allowed = |c: char| c.is_ascii_alphanumeric() || "._/~^@{}-".contains(c);
    if reference.is_empty() || reference.len() > 200 {
        return Err("`base` must name a commit, branch or tag".to_owned());
    }
    if reference.starts_with('-') || !reference.chars().all(allowed) {
        return Err(format!(
            "`base` {reference:?} is not a commit, branch or tag; use letters, digits and ._/~^@{{}}-"
        ));
    }
    Ok(reference)
}

/// `text` in single quotes, which a POSIX shell and PowerShell both take literally, or an
/// error if it holds a quote or a control character.
pub fn quote(text: &str) -> Result<String, String> {
    if text.contains('\'') || text.chars().any(char::is_control) {
        return Err(format!(
            "{text:?} cannot be quoted for the shell: it holds a quote or a control character"
        ));
    }
    Ok(format!("'{text}'"))
}

/// The paths `git ls-files --others` listed, at most [`MAX_UNTRACKED`].
pub fn untracked(listing: &str) -> Vec<String> {
    listing
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(MAX_UNTRACKED)
        .map(str::to_owned)
        .collect()
}

/// The reviewer's instructions, with the agent's own `extra` after them.
pub fn system(extra: Option<&str>) -> String {
    let mut system = String::from(
        "You review a change another coding agent made, before it reports the work done. \
         Find what is wrong, not what is fine: bugs, unhandled cases, broken or missing \
         tests, security problems, and anything that does not do what the change is meant \
         to do. For each finding give the file and line, what is wrong, why it matters and \
         the fix, most serious first. Do not restate the change or praise it. If nothing \
         needs to change, say so in one line.",
    );
    if let Some(extra) = extra.map(str::trim).filter(|extra| !extra.is_empty()) {
        system.push_str("\n\n");
        system.push_str(extra);
    }
    system
}

/// What there is to review.
pub struct Changes<'a> {
    /// What the change is meant to do, as the agent says.
    pub goal: Option<&'a str>,
    /// What the diff is against.
    pub base: &'a str,
    /// `git diff`'s output.
    pub diff: &'a str,
    /// Files to show whole: new ones and those asked for, as (path, contents).
    pub files: &'a [(String, String)],
}

/// The request for the reviewer, at most about `max_bytes` long, and how many of the files
/// were left out for want of room.
pub fn prompt(changes: &Changes<'_>, max_bytes: usize) -> (String, usize) {
    let mut text = String::new();
    if let Some(goal) = changes.goal.map(str::trim).filter(|goal| !goal.is_empty()) {
        // Writing to a `String` cannot fail.
        let _ = write!(text, "The change is meant to: {goal}\n\n");
    }
    let diff = changes.diff.trim_end();
    if diff.is_empty() {
        let _ = writeln!(text, "`git diff {}` shows nothing.", changes.base);
    } else {
        let header = format!("=== git diff {} ===\n", changes.base);
        let budget = max_bytes.saturating_sub(text.len() + header.len());
        let (shown, cut) = cut(diff, budget);
        let _ = writeln!(text, "{header}{shown}");
        if cut {
            text.push_str("[the diff was cut short here]\n");
        }
    }
    let mut left_out = 0;
    for (path, contents) in changes.files {
        let block = format!("\n=== {path} (whole file) ===\n{contents}\n");
        if text.len() + block.len() > max_bytes {
            left_out += 1;
            continue;
        }
        text.push_str(&block);
    }
    (text, left_out)
}

/// At most `budget` bytes of `text`, cut at the end of a line where there is one, and
/// whether anything was cut.
fn cut(text: &str, budget: usize) -> (&str, bool) {
    if text.len() <= budget {
        return (text, false);
    }
    let mut end = budget;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let head = text.get(..end).unwrap_or_default();
    let head = head
        .rfind('\n')
        .map_or(head, |line| head.get(..line).unwrap_or(head));
    (head, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_are_refs_and_never_options() {
        for good in [
            "HEAD",
            "main",
            "origin/main",
            "HEAD~2",
            "v1.0^",
            "main@{1}",
            "a1b2c3",
        ] {
            assert_eq!(reference(good), Ok(good));
        }
        for bad in [
            "",
            "--output=/tmp/x",
            "-p",
            "main; rm -rf /",
            "a'b",
            "a b",
            "$(id)",
        ] {
            assert!(reference(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn quoting_refuses_what_would_end_the_quote() {
        assert_eq!(quote("/work/my repo").unwrap(), "'/work/my repo'");
        assert!(quote("/work/it's").is_err());
        assert!(quote("/work/a\nb").is_err());
    }

    #[test]
    fn untracked_files_are_listed_up_to_the_limit() {
        assert_eq!(untracked("a.rs\n\n src/b.rs \n"), ["a.rs", "src/b.rs"]);
        let many = "f\n".repeat(MAX_UNTRACKED + 10);
        assert_eq!(untracked(&many).len(), MAX_UNTRACKED);
    }

    #[test]
    fn the_agents_instructions_follow_the_reviewers() {
        assert!(!system(None).contains("\n\n"));
        let system = system(Some("Check the error handling."));
        assert!(
            system.ends_with("\n\nCheck the error handling."),
            "{system}"
        );
    }

    #[test]
    fn the_request_holds_the_goal_the_diff_and_the_files() {
        let files = [("new.rs".to_owned(), "fn new() {}".to_owned())];
        let (text, left_out) = prompt(
            &Changes {
                goal: Some("add new()"),
                base: "HEAD",
                diff: "+fn changed() {}\n",
                files: &files,
            },
            10_000,
        );
        assert_eq!(left_out, 0);
        assert!(
            text.starts_with("The change is meant to: add new()\n\n"),
            "{text}"
        );
        assert!(
            text.contains("=== git diff HEAD ===\n+fn changed() {}\n"),
            "{text}"
        );
        assert!(
            text.contains("=== new.rs (whole file) ===\nfn new() {}\n"),
            "{text}"
        );
    }

    #[test]
    fn a_long_diff_is_cut_at_a_line_and_files_that_do_not_fit_are_counted() {
        let diff = "+line one\n+line two\n+line three\n";
        let files = [("big.rs".to_owned(), "x".repeat(100))];
        let (text, left_out) = prompt(
            &Changes {
                goal: None,
                base: "main",
                diff,
                files: &files,
            },
            40,
        );
        assert_eq!(left_out, 1);
        assert!(text.contains("+line one\n"), "{text}");
        assert!(!text.contains("three"), "{text}");
        assert!(text.contains("[the diff was cut short here]"), "{text}");
    }

    #[test]
    fn an_empty_diff_is_said_to_be_empty() {
        let (text, _) = prompt(
            &Changes {
                goal: None,
                base: "HEAD",
                diff: "\n",
                files: &[],
            },
            1_000,
        );
        assert_eq!(text, "`git diff HEAD` shows nothing.\n");
    }
}
