//! The one list of event topics, checked: every topic the host's code raises is in the
//! events reference, and the reference names none the code does not raise.
//!
//! A prototype of the check C11 in `docs/open-questions.md` asks for (see
//! `docs/prototypes.md`). It reads the sources and the page as text, so it finds a topic
//! written as a string literal of the form `<prefix>/<what>` under one of [`PREFIXES`], in
//! code that is not a test's. It does not find the four topics of one word (`message`,
//! `signal`, `usage`, `compaction`), and it says nothing of what a payload holds or of
//! which topics are contract.

use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

/// What the topics of the core, the loop and the standard extensions begin with. An
/// add-on's own topics (`skills/…`) are its own, and are not looked for.
const PREFIXES: [&str; 13] = [
    "session",
    "tool",
    "gate",
    "model",
    "run",
    "turn",
    "plugin",
    "capability",
    "state",
    "llm",
    "http",
    "process",
    "secret",
];

/// Literals of a topic's form that are something else: `plugin/load` is a decision point
/// that no event is named after.
const NOT_TOPICS: [&str; 1] = ["plugin/load"];

/// Whether `text` has the form of one of the harness's topics.
fn is_topic(text: &str) -> bool {
    text.split_once('/').is_some_and(|(prefix, what)| {
        PREFIXES.contains(&prefix)
            && !what.is_empty()
            && what
                .chars()
                .all(|character| character.is_ascii_lowercase() || character == '-')
    })
}

/// Every stretch of `text` between two `quote`s that has a topic's form.
fn topics_between(text: &str, quote: char, found: &mut Topics) {
    found.extend(
        text.split(quote)
            .filter(|between| is_topic(between))
            .map(str::to_owned),
    );
}

/// The Rust sources under `dir`, tests' own directories and build output left out.
fn sources(dir: &Path, found: &mut Vec<PathBuf>) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path.file_name().and_then(|name| name.to_str());
        if path.is_dir() {
            if !matches!(name, Some("tests" | "target" | "examples")) {
                sources(&path, found)?;
            }
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
    Ok(())
}

/// Topics, sorted.
type Topics = BTreeSet<String>;

/// The root of the repository.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The topics the host's code raises.
fn raised() -> Result<Topics, Box<dyn Error>> {
    let mut files = Vec::new();
    sources(&root().join("src"), &mut files)?;
    sources(&root().join("crates"), &mut files)?;
    let mut raised = Topics::new();
    for file in files {
        let text = fs::read_to_string(&file)?;
        // What follows is the file's unit tests, which make topics up.
        let code = text.split("#[cfg(test)]").next().unwrap_or_default();
        topics_between(code, '"', &mut raised);
    }
    for other in NOT_TOPICS {
        raised.remove(other);
    }
    Ok(raised)
}

/// The topics the events reference names.
fn named() -> Result<Topics, Box<dyn Error>> {
    let mut named = Topics::new();
    let page = fs::read_to_string(root().join("docs/reference/events.md"))?;
    topics_between(&page, '`', &mut named);
    Ok(named)
}

#[test]
fn the_events_reference_names_every_topic_the_code_raises_and_no_other() {
    let (raised, named) = (raised().unwrap(), named().unwrap());

    // The scan found the code: a topic of each layer is among what it read.
    for known in ["tool/start", "plugin/activated", "model/request"] {
        assert!(
            raised.contains(known),
            "{known} was not found in the sources"
        );
    }
    let unnamed: Vec<_> = raised.difference(&named).collect();
    let unraised: Vec<_> = named.difference(&raised).collect();
    assert!(
        unnamed.is_empty(),
        "raised by the code and not in docs/reference/events.md: {unnamed:?}"
    );
    assert!(
        unraised.is_empty(),
        "in docs/reference/events.md and raised by no code: {unraised:?}"
    );
}
