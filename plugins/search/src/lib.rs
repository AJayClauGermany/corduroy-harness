//! Search the workspace without a shell: `grep` and `find`, after Pi's tools of those names.
//!
//! A model finds its way through a codebase by searching it. With only `bash` to search
//! with, every search is a command, and a gate that asks before a command runs asks about
//! each one. These two tools only look: they reach files through `fs-read` alone, within
//! whatever the grant allows, so they are read-only and run without a shell at all.
//!
//! `grep` finds the lines matching a regular expression; `find` finds files by a pattern on
//! their path ([`glob`]). Both skip `.git`, `node_modules`, `target`, `.corduroy` and
//! `.venv`; `grep` also passes over files that are not text, and the agent file in the
//! working directory, which no add-on may read.
//!
//! Configuration: `max_bytes`, the most text one `grep` reads (16 MB by default). A call is
//! metered, and one that runs out of its budget takes the add-on's instance down, so a
//! search stops well short of that, says so, and the model narrows it.

mod glob;
mod lines;

use std::fmt::Write as _;
use std::ops::ControlFlow;
use std::sync::OnceLock;

use corduroy_guest::host::fs_read;
use corduroy_guest::{Plugin, PluginInfo, ToolError, ToolSpec, parse};
use regex::{Regex, RegexBuilder};
use serde::Deserialize;

use glob::Glob;

/// Directories not opened.
const UNOPENED: [&str; 5] = [".git/", "node_modules/", "target/", ".corduroy/", ".venv/"];
/// The agent file, where the workspace has one. It is out of reach of every add-on, and
/// asking for it is recorded as a refusal each time, so a search of the working directory
/// does not ask.
const AGENT_FILE: &str = "corduroy.toml";
/// Files looked at by one call, at most.
const MAX_FILES: usize = 20_000;
/// How deep below the starting directory a search goes.
const MAX_DEPTH: usize = 40;
/// Bytes of a result, at most.
const MAX_OUTPUT: usize = 50 * 1024;
/// Bytes of text one `grep` reads unless the configuration says otherwise. A call's budget
/// is spent at about 40 MB by a pattern found in every file, and far later by one found in
/// few.
const DEFAULT_BYTES: usize = 16 << 20;
/// The longest regular expression taken, in bytes.
const MAX_PATTERN: usize = 1000;
/// How large a compiled regular expression may be, in bytes: one that needs more would
/// spend the call's budget on a single file.
const MAX_PROGRAM: usize = 512 * 1024;

/// Plugin configuration, from the host.
#[derive(Deserialize)]
#[serde(default)]
struct Config {
    /// Bytes of text one `grep` reads at most.
    max_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_BYTES,
        }
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();

#[derive(Deserialize)]
struct Grep {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    #[serde(default)]
    ignore_case: bool,
    #[serde(default)]
    context: usize,
    limit: Option<usize>,
    #[serde(default)]
    files_only: bool,
}

#[derive(Deserialize)]
struct Find {
    pattern: String,
    path: Option<String>,
    limit: Option<usize>,
}

/// `name` in `dir`, where an empty `dir` or `.` is the directory paths are relative to.
fn join(dir: &str, name: &str) -> String {
    let dir = dir.trim_end_matches('/');
    if dir.is_empty() || dir == "." {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

/// What a search does with each file: go on, or stop.
type Visit<'a> = dyn FnMut(&str) -> ControlFlow<()> + 'a;

/// Where a search over a directory stands.
struct Walk<'a> {
    /// The directory the search began in.
    root: &'a str,
    /// Files seen so far.
    files: usize,
    /// Whether the search stopped at [`MAX_FILES`].
    cut: bool,
}

impl Walk<'_> {
    /// Call `visit` with each file at and below the root, as its path under the root: a
    /// directory's files in name order, then its directories the same way. Directories that
    /// cannot be listed are passed over. Stops when `visit` says so.
    fn each(&mut self, visit: &mut Visit<'_>) {
        // Directories still to open, the next one last, each with how deep it is.
        let mut open = vec![(String::new(), 0)];
        while let Some((below, depth)) = open.pop() {
            let Ok(entries) = fs_read::list(&join(self.root, &below)) else {
                continue;
            };
            let (directories, files): (Vec<String>, Vec<String>) =
                entries.into_iter().partition(|name| name.ends_with('/'));
            let mut each = files
                .iter()
                .map(|name| self.file(&join(&below, name), visit));
            if each.any(|went| went.is_break()) {
                return;
            }
            let deeper = directories
                .iter()
                .rev()
                .filter(|name| depth < MAX_DEPTH && !UNOPENED.contains(&name.as_str()))
                .map(|name| (join(&below, name.trim_end_matches('/')), depth + 1));
            open.extend(deeper);
        }
    }

    /// Count one file and hand it to `visit`, unless that is one file too many.
    fn file(&mut self, path: &str, visit: &mut Visit<'_>) -> ControlFlow<()> {
        if self.files >= MAX_FILES {
            self.cut = true;
            return ControlFlow::Break(());
        }
        self.files += 1;
        visit(path)
    }
}

/// The directory a call searches: its `path`, or the one paths are relative to.
fn start(path: Option<&str>) -> &str {
    path.map(str::trim)
        .filter(|path| !path.is_empty())
        .unwrap_or(".")
}

fn find(input: &Find) -> Result<String, ToolError> {
    let pattern = Glob::new(&input.pattern).map_err(ToolError::InvalidInput)?;
    let root = start(input.path.as_deref());
    // A directory that cannot be listed is the model's mistake to hear about.
    fs_read::list(root)?;
    let limit = input.limit.unwrap_or(200).clamp(1, 2000);
    let mut found = Vec::new();
    let mut more = false;
    let mut walk = Walk {
        root,
        files: 0,
        cut: false,
    };
    walk.each(&mut |file| {
        if !pattern.matches(file) {
            return ControlFlow::Continue(());
        }
        if found.len() == limit {
            more = true;
            return ControlFlow::Break(());
        }
        found.push(join(root, file));
        ControlFlow::Continue(())
    });
    if found.is_empty() {
        return Ok(format!(
            "No file under `{root}` matches `{}` ({} looked at).",
            input.pattern, walk.files
        ));
    }
    let mut text = found.join("\n");
    if more {
        // Writing to a `String` cannot fail.
        let _ = write!(
            text,
            "\n[the first {limit} are shown: narrow the pattern or the path, or raise `limit`]"
        );
    } else if walk.cut {
        let _ = write!(
            text,
            "\n[stopped after {MAX_FILES} files: search a directory further down]"
        );
    }
    Ok(text)
}

/// The regular expression of a `grep`, in two forms.
struct Pattern {
    /// Matched against each line.
    line: Regex,
    /// Matched against a whole file first, where that can be done without missing a line:
    /// most files hold no match, and saying so for the file at once costs far less of the
    /// call's budget than asking of every line.
    file: Option<Regex>,
}

impl Pattern {
    fn new(input: &Grep) -> Result<Self, ToolError> {
        if input.pattern.is_empty() || input.pattern.len() > MAX_PATTERN {
            return Err(ToolError::InvalidInput(format!(
                "`pattern` must be a regular expression of 1 to {MAX_PATTERN} bytes"
            )));
        }
        let build = |whole: bool| {
            RegexBuilder::new(&input.pattern)
                .case_insensitive(input.ignore_case)
                .multi_line(whole)
                .size_limit(MAX_PROGRAM)
                .build()
        };
        let line =
            build(false).map_err(|error| ToolError::InvalidInput(format!("`pattern`: {error}")))?;
        let file = whole_file(&input.pattern)
            .then(|| build(true).ok())
            .flatten();
        Ok(Self { line, file })
    }

    /// Whether no line of `contents` can match, as far as one look at the whole says.
    fn rules_out(&self, contents: &str) -> bool {
        // A line may end in a carriage return, which the whole-file form does not know.
        self.file
            .as_ref()
            .is_some_and(|file| !contents.contains('\r') && !file.is_match(contents))
    }
}

/// Whether `pattern`, matched against a whole file with `^` and `$` at each line's ends,
/// matches wherever it matches one of the file's lines. It does unless it speaks of the
/// start or end of the text (`\A`, `\z`) or may switch flags off, the line anchors' among
/// them (`(?-m)`).
fn whole_file(pattern: &str) -> bool {
    !(pattern.contains("\\A")
        || pattern.contains("\\z")
        || (pattern.contains("(?") && pattern.contains('-')))
}

/// What a `grep` has found so far, and why it stopped if it did.
struct Search<'a> {
    input: &'a Grep,
    pattern: Pattern,
    limit: usize,
    /// Bytes that may still be read.
    budget: usize,
    text: String,
    matches: usize,
    /// Files read.
    read: usize,
    stopped: Option<String>,
}

impl Search<'_> {
    /// Search the file at `path`. Breaks when the search is over.
    fn file(&mut self, path: &str) -> ControlFlow<()> {
        // A file the grant does not reach, or that is not text, is passed over.
        let Ok(contents) = fs_read::read(path) else {
            return ControlFlow::Continue(());
        };
        if contents.len() > self.budget {
            self.stopped = Some(format!(
                "stopped after reading {} files: the rest is more text than one search \
                 reads. Search a directory further down, or name the files with `glob`",
                self.read
            ));
            return ControlFlow::Break(());
        }
        self.budget -= contents.len();
        self.read += 1;
        if self.pattern.rules_out(&contents) {
            return ControlFlow::Continue(());
        }
        let left = self.limit.saturating_sub(self.matches);
        let line = &self.pattern.line;
        let found = lines::scan(
            path,
            &contents,
            |text| line.is_match(text),
            self.input.context,
            left,
        );
        if found.matches == 0 {
            return ControlFlow::Continue(());
        }
        self.matches += found.matches;
        if self.input.files_only {
            // Writing to a `String` cannot fail.
            let _ = writeln!(self.text, "{path}");
        } else {
            if self.input.context > 0 && !self.text.is_empty() {
                self.text.push_str("--\n");
            }
            self.text.push_str(&found.text);
        }
        if self.matches >= self.limit {
            self.stopped = Some(format!(
                "stopped at {} matching lines: narrow the pattern or the path, or raise `limit`",
                self.matches
            ));
        } else if self.text.len() > MAX_OUTPUT {
            self.stopped = Some("stopped: the result is long enough. Narrow the search".to_owned());
        }
        if self.stopped.is_some() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    }
}

fn grep(input: &Grep) -> Result<String, ToolError> {
    let only = input
        .glob
        .as_deref()
        .map(Glob::new)
        .transpose()
        .map_err(ToolError::InvalidInput)?;
    let root = start(input.path.as_deref());
    let mut search = Search {
        input,
        pattern: Pattern::new(input)?,
        limit: input.limit.unwrap_or(100).clamp(1, 1000),
        budget: CONFIG
            .get()
            .map_or(DEFAULT_BYTES, |config| config.max_bytes),
        text: String::new(),
        matches: 0,
        read: 0,
        stopped: None,
    };
    let mut walk = Walk {
        root,
        files: 0,
        cut: false,
    };
    if fs_read::list(root).is_ok() {
        walk.each(&mut |file| {
            let passed_over = only.as_ref().is_some_and(|only| !only.matches(file))
                || (root == "." && file == AGENT_FILE);
            if passed_over {
                return ControlFlow::Continue(());
            }
            search.file(&join(root, file))
        });
    } else {
        // Not a directory: one file, and what is wrong with it is the model's to hear.
        fs_read::read(root)?;
        let _stopped = search.file(root);
    }
    if walk.cut && search.stopped.is_none() {
        search.stopped = Some(format!(
            "stopped after {MAX_FILES} files: search a directory further down"
        ));
    }
    let mut text = search.text;
    if text.is_empty() {
        // Writing to a `String` cannot fail.
        let _ = write!(
            text,
            "Nothing matches `{}` in the {} file(s) read under `{root}`.",
            input.pattern, search.read
        );
    }
    if let Some(stopped) = search.stopped {
        let _ = write!(text, "\n[{stopped}]");
    }
    Ok(text.trim_end().to_owned())
}

struct SearchTools;

impl Plugin for SearchTools {
    fn activate(config: String) -> Result<PluginInfo, String> {
        let config: Config = corduroy_guest::config(&config)?;
        // A second activation in the same instance keeps the first configuration.
        CONFIG.get_or_init(|| config);
        Ok(PluginInfo::new("search", env!("CARGO_PKG_VERSION"))
            .tool(
                ToolSpec::new(
                    "grep",
                    "Find the lines of files that match a regular expression, as \
                     `path:line:text`. Searches `path` (a directory, searched all the way \
                     down, or one file; the working directory by default), in files whose \
                     path matches `glob` if given. Prefer this to running grep in a shell.",
                    r#"{"type":"object","properties":{"pattern":{"type":"string","description":"A regular expression, matched against each line"},"path":{"type":"string","description":"The directory or file to search (default: the working directory)"},"glob":{"type":"string","description":"Search only files matching this, such as *.rs or src/**/*.{ts,tsx}"},"ignore_case":{"type":"boolean"},"context":{"type":"integer","description":"Lines to show before and after each match (default 0)"},"limit":{"type":"integer","description":"Most matching lines (default 100)"},"files_only":{"type":"boolean","description":"List the files with a match instead of the lines"}},"required":["pattern"]}"#,
                )
                .read_only()
                .replay_safe(),
            )
            .tool(
                ToolSpec::new(
                    "find",
                    "Find files by a pattern on their path, such as *.rs (that name at any \
                     depth), src/**/*.toml or **/tests/*.{py,sh}. `*` stays within one name, \
                     `**` crosses directories. Lists paths in name order.",
                    r#"{"type":"object","properties":{"pattern":{"type":"string","description":"A file-name pattern; without a / it is matched against the name alone"},"path":{"type":"string","description":"The directory to search (default: the working directory)"},"limit":{"type":"integer","description":"Most files listed (default 200)"}},"required":["pattern"]}"#,
                )
                .read_only()
                .replay_safe(),
            ))
    }

    fn call_tool(name: String, input: String) -> Result<String, ToolError> {
        match name.as_str() {
            "grep" => grep(&parse(&input)?),
            "find" => find(&parse(&input)?),
            _ => Err(ToolError::UnknownTool(name)),
        }
    }
}

corduroy_guest::export!(SearchTools);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_joined_without_a_leading_dot() {
        assert_eq!(join("", "a.rs"), "a.rs");
        assert_eq!(join(".", "a.rs"), "a.rs");
        assert_eq!(join("src/", "a.rs"), "src/a.rs");
        assert_eq!(join("/work/src", "a.rs"), "/work/src/a.rs");
    }

    #[test]
    fn a_whole_file_is_asked_first_only_where_that_misses_no_line() {
        let grep = |pattern: &str| Grep {
            pattern: pattern.to_owned(),
            path: None,
            glob: None,
            ignore_case: false,
            context: 0,
            limit: None,
            files_only: false,
        };
        let ends = Pattern::new(&grep(";$")).unwrap();
        // `$` is each line's end, so a line in the middle is not ruled out.
        assert!(!ends.rules_out("a;\nb\n"));
        assert!(ends.rules_out("a\nb\n"));
        // A carriage return before the line's end: ask each line.
        assert!(!ends.rules_out("a\r\nb\r\n"));
        for pattern in ["\\Afn", "end\\z", "(?-m)x$", "(?i-s)x"] {
            assert!(
                Pattern::new(&grep(pattern)).unwrap().file.is_none(),
                "{pattern}"
            );
        }
        assert!(
            Pattern::new(&grep("(?i)todo|fixme"))
                .unwrap()
                .file
                .is_some()
        );
    }

    #[test]
    fn a_search_starts_in_the_working_directory_unless_told() {
        assert_eq!(start(None), ".");
        assert_eq!(start(Some("  ")), ".");
        assert_eq!(start(Some("src")), "src");
    }
}
