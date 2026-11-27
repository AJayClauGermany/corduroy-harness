//! File-name patterns: `*.rs`, `src/**/*.toml`, `*.{ts,tsx}`. Pure, so it is tested natively.
//!
//! | In a pattern | Matches |
//! | --- | --- |
//! | `*` | any run of characters within one name |
//! | `**` | any run of characters, `/` included; `**/` is any number of directories |
//! | `?` | one character of a name |
//! | `{a,b}` | either `a` or `b` |
//!
//! Nothing else is syntax. A `[`, which other tools read as the start of a set of characters
//! (`[0-9]`), is refused with what to write instead, where matching it as itself would
//! find nothing and say nothing.
//!
//! A pattern with no `/` in it is matched against a file's name alone, so `*.rs` finds
//! Rust files at any depth; one with a `/` is matched against the whole path.

/// The most alternatives one pattern's braces may expand to.
const MAX_ALTERNATIVES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Literal(char),
    /// `?`
    One,
    /// `*`
    Run,
    /// `**`
    Any,
    /// `**/`
    Directories,
}

/// A pattern, ready to match paths against.
#[derive(Debug)]
pub struct Glob {
    alternatives: Vec<Vec<Token>>,
    /// Whether the pattern names no directory, and so is matched against the name alone.
    name_only: bool,
}

impl Glob {
    /// The pattern `text`.
    ///
    /// # Errors
    ///
    /// Says what is wrong with it: it is empty, it uses a set of characters (`[a-z]`), which
    /// is not supported, a brace is not closed, or its braces expand to too many
    /// alternatives.
    pub fn new(text: &str) -> Result<Self, String> {
        let text = text.trim().trim_start_matches("./");
        if text.is_empty() {
            return Err("the pattern is empty".to_owned());
        }
        if text.contains('[') {
            return Err(format!(
                "unsupported pattern `{text}`: sets of characters such as `[0-9]` are not \
                 supported. A pattern takes `*`, `**`, `?` and `{{a,b}}`: write `?` for any \
                 one character, or each case out, as in `file{{1,2,3}}.txt`"
            ));
        }
        let alternatives = expand(text)?
            .iter()
            .map(|alternative| tokens(alternative))
            .collect();
        Ok(Self {
            alternatives,
            name_only: !text.contains('/'),
        })
    }

    /// Whether `path`, a file's path with `/` between its names, matches.
    #[must_use]
    pub fn matches(&self, path: &str) -> bool {
        let subject = if self.name_only {
            path.rsplit('/').next().unwrap_or(path)
        } else {
            path
        };
        let text: Vec<char> = subject.chars().collect();
        self.alternatives.iter().any(|tokens| {
            let mut seen = vec![None; (tokens.len() + 1) * (text.len() + 1)];
            matches(tokens, &text, 0, 0, &mut seen)
        })
    }
}

/// `text` with each `{a,b}` written out: every alternative it stands for.
fn expand(text: &str) -> Result<Vec<String>, String> {
    let Some((before, rest)) = text.split_once('{') else {
        return Ok(vec![text.to_owned()]);
    };
    let Some((choices, after)) = rest.split_once('}') else {
        return Err(format!("`{text}` has a `{{` that is never closed"));
    };
    let mut expanded = Vec::new();
    for tail in expand(after)? {
        for choice in choices.split(',') {
            expanded.push(format!("{before}{choice}{tail}"));
        }
    }
    if expanded.len() > MAX_ALTERNATIVES {
        return Err(format!(
            "`{text}` stands for more than {MAX_ALTERNATIVES} patterns"
        ));
    }
    Ok(expanded)
}

fn tokens(pattern: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        let token = match c {
            '?' => Token::One,
            '*' if chars.peek() == Some(&'*') => {
                while chars.peek() == Some(&'*') {
                    chars.next();
                }
                if chars.peek() == Some(&'/') {
                    chars.next();
                    Token::Directories
                } else {
                    Token::Any
                }
            }
            '*' => Token::Run,
            other => Token::Literal(other),
        };
        tokens.push(token);
    }
    tokens
}

/// Whether `tokens` from `at` match `text` from `from`. `seen` remembers each answer, so a
/// pattern with several stars does not try the same place twice.
fn matches(
    tokens: &[Token],
    text: &[char],
    at: usize,
    from: usize,
    seen: &mut Vec<Option<bool>>,
) -> bool {
    let slot = at * (text.len() + 1) + from;
    if let Some(Some(known)) = seen.get(slot) {
        return *known;
    }
    let next = text.get(from).copied();
    let answer = match tokens.get(at) {
        None => next.is_none(),
        Some(Token::Literal(c)) => {
            next == Some(*c) && matches(tokens, text, at + 1, from + 1, seen)
        }
        Some(Token::One) => {
            next.is_some_and(|c| c != '/') && matches(tokens, text, at + 1, from + 1, seen)
        }
        Some(Token::Run) => {
            matches(tokens, text, at + 1, from, seen)
                || (next.is_some_and(|c| c != '/') && matches(tokens, text, at, from + 1, seen))
        }
        Some(Token::Any) => {
            matches(tokens, text, at + 1, from, seen)
                || (next.is_some() && matches(tokens, text, at, from + 1, seen))
        }
        Some(Token::Directories) => {
            matches(tokens, text, at + 1, from, seen)
                || text
                    .iter()
                    .skip(from)
                    .position(|c| *c == '/')
                    .is_some_and(|slash| matches(tokens, text, at, from + slash + 1, seen))
        }
    };
    if let Some(known) = seen.get_mut(slot) {
        *known = Some(answer);
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glob(pattern: &str) -> Glob {
        Glob::new(pattern).unwrap()
    }

    #[test]
    fn a_pattern_without_a_slash_is_matched_against_the_name() {
        let rust = glob("*.rs");
        assert!(rust.matches("lib.rs"));
        assert!(rust.matches("src/plugin/host.rs"));
        assert!(!rust.matches("src/rs/notes.md"));
        assert!(glob("Cargo.toml").matches("plugins/Cargo.toml"));
    }

    #[test]
    fn a_star_stays_within_one_name_and_two_cross_directories() {
        assert!(glob("src/*.rs").matches("src/lib.rs"));
        assert!(!glob("src/*.rs").matches("src/plugin/host.rs"));
        assert!(glob("src/**/*.rs").matches("src/lib.rs"));
        assert!(glob("src/**/*.rs").matches("src/plugin/deep/host.rs"));
        assert!(!glob("src/**/*.rs").matches("tests/src.rs"));
        assert!(glob("**/tests/*.rs").matches("tests/a.rs"));
        assert!(glob("**/tests/*.rs").matches("crates/x/tests/a.rs"));
        assert!(!glob("**/tests/*.rs").matches("crates/x/contests/a.rs"));
        assert!(glob("docs/**").matches("docs/how-to/x.md"));
    }

    #[test]
    fn a_question_mark_is_one_character_and_braces_are_alternatives() {
        assert!(glob("v?.txt").matches("v1.txt"));
        assert!(!glob("v?.txt").matches("v10.txt"));
        let web = glob("*.{ts,tsx,css}");
        assert!(web.matches("app/page.tsx"));
        assert!(web.matches("app/site.css"));
        assert!(!web.matches("app/page.js"));
        assert!(glob("./src/*.rs").matches("src/lib.rs"));
    }

    #[test]
    fn a_pattern_that_cannot_be_read_says_why() {
        assert!(Glob::new(" ").unwrap_err().contains("empty"));
        assert!(Glob::new("*.{rs").unwrap_err().contains("never closed"));
        // A set of characters would match nothing as written, and say nothing.
        let set = Glob::new("chapter[1-8].md").unwrap_err();
        assert!(set.starts_with("unsupported pattern `chapter[1-8].md`"));
        assert!(set.contains("`?`") && set.contains("{a,b}"), "{set}");
        let many = "{a,b,c,d,e,f,g,h,i}{a,b,c,d,e,f,g,h,i}";
        assert!(Glob::new(many).unwrap_err().contains("more than 64"));
    }

    #[test]
    fn many_stars_do_not_take_forever() {
        let pattern = glob("*a*a*a*a*a*a*a*a*a*a*b");
        assert!(!pattern.matches(&"a".repeat(200)));
    }
}
