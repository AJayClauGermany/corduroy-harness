//! An append-only JSON Lines file as the durable log behind an event stream.
//!
//! The core defines what a log must do ([`Log`]) and stores nothing itself; this crate is one
//! store. The file is plain text so a run can be read, searched and repaired by hand:
//!
//! ```text
//! {"corduroy-log":2}
//! [{"id":1,"time":1795368645804,"session":"main","source":"user","cause":null,"topic":"message","payload":"..."}]
//! [{"id":2,"time":1795368647113,"session":"main","source":"agent","cause":null,"topic":"message","payload":"..."},{"id":3,...}]
//! ```
//!
//! The first line is the header and carries the schema version. Version 2 added `time`
//! (milliseconds since the Unix epoch) to each event; a version 1 log is still read, its
//! events having time 0, and is continued as it is. Every line after the header is one
//! commit: the events of one step, written with a single `write` and flushed to the device
//! before the stream shows them to anyone. A crash can therefore leave at most one unfinished
//! last line, which [`JsonlLog::open`] cuts off, so a commit is in the log whole or not at all.
//!
//! The file is locked while it is open: a log has one writer.
//!
//! While a stream writes to it the file only grows. Between runs, [`remove`] takes whole
//! sessions out of it, as maintenance a person asks for: the file is written again without
//! them, and with a commit recording their removal, then put in place of the old one in one
//! step. Ids are never used again, so a log may have gaps in them.
//!
//! ```
//! # let path = std::env::temp_dir().join(format!("corduroy-jsonl-doc-{}.jsonl", std::process::id()));
//! # drop(std::fs::remove_file(&path));
//! use corduroy_core::{Draft, Source};
//!
//! let stream = corduroy_jsonl::open(&path)?;
//! stream.emit(Draft::new(Source::User, "message").payload("hi"))?;
//! drop(stream);
//!
//! // Later, perhaps after a crash: the same events, and numbering carries on.
//! let stream = corduroy_jsonl::open(&path)?;
//! assert_eq!(stream.len(), 1);
//! assert_eq!(stream.emit(Draft::new(Source::User, "message"))?.get(), 2);
//! # drop(stream);
//! # drop(std::fs::remove_file(&path));
//! # Ok::<(), corduroy_core::LogError>(())
//! ```

use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

use corduroy_core::{Draft, Event, EventId, EventStream, Log, LogError, SessionId};
use serde_json::{Value, json};

/// The schema version this crate writes.
pub const SCHEMA_VERSION: u64 = 2;

/// The oldest schema version this crate still reads.
pub const OLDEST_READ: u64 = 1;

/// The key of the header line, whose value is the schema version.
const HEADER_KEY: &str = "corduroy-log";

/// Open the log at `path`, creating it if it does not exist, as an event stream holding
/// everything the log already does. Every later commit is stored there first.
///
/// # Errors
///
/// As [`JsonlLog::open`].
pub fn open(path: impl AsRef<Path>) -> Result<EventStream, LogError> {
    let (log, events) = JsonlLog::open(path)?;
    Ok(EventStream::load(events).with_log(log))
}

/// A JSON Lines file holding one commit per line. See the [crate documentation](crate).
pub struct JsonlLog {
    file: File,
    /// The id of the last event stored, to check that commits arrive in order.
    last: u64,
}

impl fmt::Debug for JsonlLog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JsonlLog")
            .field("last", &self.last)
            .finish_non_exhaustive()
    }
}

impl JsonlLog {
    /// Open the log at `path`, creating it if it does not exist, and read back its events.
    ///
    /// An unfinished last line, left by a crash in the middle of a write, is removed.
    ///
    /// # Errors
    ///
    /// Returns a [`LogError`] if the file cannot be opened, read or locked (another writer has
    /// it), is not a corduroy log, has a schema version outside [`OLDEST_READ`] to
    /// [`SCHEMA_VERSION`], or holds a complete line that is not a valid commit.
    pub fn open(path: impl AsRef<Path>) -> Result<(Self, Vec<Event>), LogError> {
        let Opened { file, commits } = open_locked(path.as_ref())?;
        let events: Vec<Event> = commits.into_iter().flatten().collect();
        let last = events.last().map_or(0, |event| event.id.get());
        Ok((Self { file, last }, events))
    }
}

/// A log file opened and locked, and what it holds.
struct Opened {
    /// The file, locked.
    file: File,
    /// Its commits, in order, without an unfinished last line.
    commits: Vec<Vec<Event>>,
}

/// Open, lock and read the log at `path`, creating it if it does not exist and cutting off
/// an unfinished last line.
fn open_locked(path: &Path) -> Result<Opened, LogError> {
    let fail = |what: &str, error: &dyn fmt::Display| {
        LogError::new(format!("{what} {}: {error}", path.display()))
    };
    // Written, not appended: on Windows a handle opened to append cannot be cut short. Writes
    // still land at the end, as this is the only writer (the lock) and where a cut is made
    // the position follows it.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| fail("cannot open", &error))?;
    file.try_lock()
        .map_err(|error| fail("cannot lock", &error))?;
    let bytes = read_locked(&mut file).map_err(|error| fail("cannot read", &error))?;

    // Everything after the last newline is a write that never finished. It may end in the
    // middle of a character, so it is set aside before the rest is read as text.
    let kept = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |newline| newline + 1);
    let (whole, torn) = bytes.split_at_checked(kept).unwrap_or((&[], &bytes));
    let whole = str::from_utf8(whole).map_err(|error| fail("cannot read", &error))?;
    let mut lines = whole.lines();
    let Some(first) = lines.next() else {
        if !header().as_bytes().starts_with(torn) {
            return Err(fail("cannot use", &"it is not a corduroy log"));
        }
        cut(&mut file, 0)
            .and_then(|()| file.write_all(format!("{}\n", header()).as_bytes()))
            .and_then(|()| file.sync_data())
            .map_err(|error| fail("cannot start", &error))?;
        return Ok(Opened {
            file,
            commits: Vec::new(),
        });
    };
    check_header(first).map_err(|error| fail("cannot use", &error))?;

    let mut commits: Vec<Vec<Event>> = Vec::new();
    let mut last = 0;
    for (number, line) in lines.enumerate() {
        let commit = parse_commit(line, last)
            .map_err(|error| fail(&format!("line {} of", number + 2), &error))?;
        last = commit.last().map_or(last, |event| event.id.get());
        commits.push(commit);
    }
    if !torn.is_empty() {
        cut(&mut file, kept as u64)
            .and_then(|()| file.sync_data())
            .map_err(|error| fail("cannot repair", &error))?;
    }
    Ok(Opened { file, commits })
}

/// The header line this crate writes.
fn header() -> String {
    format!("{}", json!({HEADER_KEY: SCHEMA_VERSION}))
}

/// Take whole sessions out of the log at `path`, between runs: nothing may have it open.
///
/// `choose` is shown every event the log holds and returns the sessions to remove and the
/// events recording their removal (a marker for each, as `corduroy_harness::removal` gives
/// them). The log is written
/// again beside the old one without those sessions' events, each commit keeping what is
/// left of it, then with the markers as one more commit numbered after every event the log
/// held, so no id is used twice. The new file then takes the old one's place in one step:
/// a crash leaves one or the other, never a mix. Nothing is written if `choose` picks no
/// session.
///
/// Returns the sessions removed.
///
/// # Errors
///
/// Returns a [`LogError`] if the log cannot be opened as [`JsonlLog::open`] would, if
/// `choose` fails, or if the new file cannot be written or put in place; the log is then as
/// it was.
pub fn remove(
    path: impl AsRef<Path>,
    choose: impl FnOnce(&EventStream) -> Result<(BTreeSet<SessionId>, Vec<Draft>), LogError>,
) -> Result<BTreeSet<SessionId>, LogError> {
    let path = path.as_ref();
    let Opened {
        file: held,
        commits,
    } = open_locked(path)?;
    let all: Vec<Event> = commits.iter().flatten().cloned().collect();
    let (removed, markers) = choose(&EventStream::load(all))?;
    if removed.is_empty() {
        return Ok(removed);
    }
    let next = write_without(path, commits, &removed, markers)?;
    fs::rename(&next, path).map_err(|error| {
        LogError::new(format!("cannot put {} in place: {error}", next.display()))
    })?;
    sync_directory(path);
    // The old file, and its lock, go only once the new one is in its place.
    drop(held);
    Ok(removed)
}

/// Write the log at `path` again beside it, as [`remove`] puts it in place: `commits`
/// without the events of `removed`, then `markers` as one commit numbered after the last
/// event of `commits`. Returns where it was written.
fn write_without(
    path: &Path,
    commits: Vec<Vec<Event>>,
    removed: &BTreeSet<SessionId>,
    markers: Vec<Draft>,
) -> Result<PathBuf, LogError> {
    let (last, time) = commits
        .iter()
        .flatten()
        .last()
        .map_or((0, 0), |event| (event.id.get(), event.time));
    let gone = |event: &Event| {
        event
            .session
            .as_ref()
            .is_some_and(|session| removed.contains(session))
    };
    let kept: Vec<Vec<Event>> = commits
        .into_iter()
        .map(|commit| commit.into_iter().filter(|event| !gone(event)).collect())
        .filter(|commit: &Vec<Event>| !commit.is_empty())
        .collect();
    let next = beside(path)?;
    let fail = |what: &str, error: &dyn fmt::Display| {
        LogError::new(format!("{what} {}: {error}", next.display()))
    };
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&next)
        .map_err(|error| fail("cannot open", &error))?;
    file.try_lock()
        .map_err(|error| fail("cannot lock", &error))?;
    let mut text = format!("{}\n", header());
    for commit in &kept {
        text.push_str(&line_of(commit).to_string());
        text.push('\n');
    }
    cut(&mut file, 0)
        .and_then(|()| file.write_all(text.as_bytes()))
        .and_then(|()| file.sync_data())
        .map_err(|error| fail("cannot write", &error))?;
    // Numbered after the last event the old log held, removed or not, so no id comes back.
    let stream = EventStream::load(kept.into_iter().flatten())
        .continuing(EventId::new(last), time)
        .with_log(JsonlLog { file, last });
    stream.commit(markers)?;
    Ok(next)
}

/// Where a log at `path` is written again before it takes the old one's place.
fn beside(path: &Path) -> Result<PathBuf, LogError> {
    let name = path
        .file_name()
        .ok_or_else(|| LogError::new(format!("{} names no file", path.display())))?;
    let mut name = name.to_owned();
    name.push(".removing");
    Ok(path.with_file_name(name))
}

/// Make a file's new name in the directory holding `path` survive a crash.
#[cfg(unix)]
fn sync_directory(path: &Path) {
    if let Some(directory) = path.parent().and_then(|parent| File::open(parent).ok()) {
        drop(directory.sync_all());
    }
}

/// Elsewhere a directory cannot be opened to be synced, and the rename is the file system's
/// to keep.
#[cfg(not(unix))]
const fn sync_directory(_path: &Path) {}

/// One commit as the line that stores it, without its newline.
fn line_of(batch: &[Event]) -> Value {
    Value::Array(
        batch
            .iter()
            .map(|event| {
                json!({
                    "id": event.id.get(),
                    "time": event.time,
                    "session": event.session.as_ref().map(SessionId::as_str),
                    "source": event.source.to_string(),
                    "cause": event.cause.map(EventId::get),
                    "topic": event.topic,
                    "payload": event.payload,
                })
            })
            .collect(),
    )
}

impl Log for JsonlLog {
    fn commit(&mut self, batch: &[Event]) -> Result<(), LogError> {
        let mut last = self.last;
        for event in batch {
            if event.id.get() != last + 1 {
                return Err(LogError::new(format!(
                    "event {} does not follow {last}, the last one stored",
                    event.id
                )));
            }
            last += 1;
        }
        // One write for the whole commit, so a crash cannot store only some of its events.
        let line = format!("{}\n", line_of(batch));
        self.file
            .write_all(line.as_bytes())
            .and_then(|()| self.file.sync_data())
            .map_err(|error| LogError::new(format!("cannot store a commit: {error}")))?;
        self.last = last;
        Ok(())
    }
}

/// Cut `file` to `len` bytes and write from there on, so that nothing written next leaves a
/// gap where the cut-off bytes were.
fn cut(file: &mut File, len: u64) -> io::Result<()> {
    file.set_len(len)?;
    file.seek(SeekFrom::Start(len)).map(drop)
}

/// The whole file, read through the handle that holds the lock.
#[expect(
    clippy::verbose_file_reads,
    reason = "reading by path could read a different file from the one that is locked"
)]
fn read_locked(file: &mut File) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Check that `line` is the header of a log this crate can read.
fn check_header(line: &str) -> Result<(), String> {
    let version = serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|header| header.get(HEADER_KEY).and_then(Value::as_u64))
        .ok_or("it is not a corduroy log")?;
    if (OLDEST_READ..=SCHEMA_VERSION).contains(&version) {
        Ok(())
    } else {
        Err(format!(
            "it has schema version {version}, and this build reads versions {OLDEST_READ} \
             to {SCHEMA_VERSION}"
        ))
    }
}

/// Decode one line into the events of a commit, whose ids must rise from above `last`. They
/// may skip some, where whole sessions were removed.
fn parse_commit(line: &str, mut last: u64) -> Result<Vec<Event>, String> {
    let commit: Vec<Value> = serde_json::from_str(line).map_err(|error| error.to_string())?;
    let mut events = Vec::with_capacity(commit.len());
    for entry in commit {
        let text = |field: &str| {
            entry
                .get(field)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("an event has no `{field}`"))
        };
        let id = entry
            .get("id")
            .and_then(Value::as_u64)
            .ok_or("an event has no `id`")?;
        if id <= last {
            return Err(format!("event #{id} does not follow #{last}"));
        }
        last = id;
        events.push(Event {
            id: EventId::new(id),
            // Version 1 had no times.
            time: entry.get("time").and_then(Value::as_u64).unwrap_or(0),
            session: entry
                .get("session")
                .and_then(Value::as_str)
                .map(SessionId::new),
            source: text("source")?
                .parse()
                .map_err(|error: LogError| error.to_string())?,
            cause: entry.get("cause").and_then(Value::as_u64).map(EventId::new),
            topic: text("topic")?.to_owned(),
            payload: text("payload")?.to_owned(),
        });
    }
    Ok(events)
}

#[cfg(test)]
mod tests {

    use corduroy_core::Source;
    use corduroy_test_support::scratch;

    use super::*;

    fn log_in(test: &str) -> PathBuf {
        scratch(test).join("run.jsonl")
    }

    fn user(topic: &str) -> Draft {
        Draft::new(Source::User, topic)
    }

    #[test]
    fn a_reopened_log_holds_the_same_events_and_continues_them() {
        let path = log_in("jsonl-reopen");
        let stream = open(&path).unwrap();
        let first = stream
            .emit(
                user("message")
                    .session(SessionId::new("main"))
                    .payload("{\"text\":\"hi\\n\"}"),
            )
            .unwrap();
        stream
            .commit([
                Draft::new(Source::Plugin("skills".into()), "skills/saved").cause(first),
                Draft::new(Source::Agent, "tool/call").cause_in_batch(0),
            ])
            .unwrap();
        let written = stream.events();
        drop(stream);

        let stream = open(&path).unwrap();

        assert_eq!(stream.events(), written);
        assert_eq!(stream.emit(user("next")).unwrap().get(), 4);
        // Read once the log lets go of it: on Windows a locked file cannot be read by another.
        drop(stream);
        let text = fs::read_to_string(&path).unwrap();
        // A header, then one line per commit.
        assert_eq!(text.lines().count(), 4);
        assert_eq!(text.lines().next(), Some(r#"{"corduroy-log":2}"#));
    }

    #[test]
    fn an_unfinished_last_line_is_cut_off() {
        let path = log_in("jsonl-torn");
        let stream = open(&path).unwrap();
        stream.emit(user("kept")).unwrap();
        drop(stream);
        let whole = fs::read_to_string(&path).unwrap();
        // Cut in the middle of a commit, and in the middle of a character of it.
        let mut torn = format!("{whole}[{{\"id\":2,\"source\":\"us").into_bytes();
        torn.extend_from_slice(&"é".as_bytes()[..1]);
        fs::write(&path, torn).unwrap();

        let stream = open(&path).unwrap();

        assert_eq!(stream.len(), 1);
        assert_eq!(stream.emit(user("next")).unwrap().get(), 2);
        // Read once the log lets go of it: on Windows a locked file cannot be read by another.
        drop(stream);
        // The unfinished line is gone, and the next commit was written where it began, with
        // no gap: what was kept, then one line.
        let text = fs::read_to_string(&path).unwrap();
        let after = text.strip_prefix(whole.as_str()).unwrap();
        assert_eq!(after.lines().count(), 1, "{after:?}");
        assert!(after.starts_with("[{"), "{after:?}");
        assert_eq!(open(&path).unwrap().len(), 2);
    }

    #[test]
    fn a_log_cut_off_in_its_header_starts_again() {
        let path = log_in("jsonl-torn-header");
        fs::write(&path, r#"{"corduro"#).unwrap();

        let stream = open(&path).unwrap();
        stream.emit(user("first")).unwrap();
        drop(stream);

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(r#"{"corduroy-log":2}"#), "{text:?}");
        assert_eq!(open(&path).unwrap().len(), 1);
    }

    #[test]
    fn a_log_has_one_writer() {
        let path = log_in("jsonl-writer");
        let _held = open(&path).unwrap();

        let error = open(&path).unwrap_err();

        assert!(error.to_string().contains("cannot lock"), "{error}");
    }

    #[test]
    fn a_version_one_log_is_read_without_times_and_continued() {
        let path = log_in("jsonl-v1");
        let old = "{\"corduroy-log\":1}\n[{\"id\":1,\"source\":\"user\",\"topic\":\"a\",\"payload\":\"\"}]\n";
        fs::write(&path, old).unwrap();

        let stream = open(&path).unwrap();
        assert_eq!(stream.events()[0].time, 0);
        stream.emit(user("b")).unwrap();
        let stored = stream.events()[1].time;
        drop(stream);

        assert!(stored > 0);
        assert_eq!(open(&path).unwrap().events()[1].time, stored);
    }

    /// The markers removing `sessions` leave, as the harness writes them.
    fn markers(sessions: &[&str]) -> (BTreeSet<SessionId>, Vec<Draft>) {
        let drafts = sessions
            .iter()
            .map(|session| Draft::new(Source::User, "session/removed").payload(*session))
            .collect();
        (
            sessions.iter().copied().map(SessionId::new).collect(),
            drafts,
        )
    }

    #[test]
    fn removing_a_session_keeps_the_rest_and_never_reuses_an_id() {
        let path = log_in("jsonl-remove");
        let (main, side) = (SessionId::new("main"), SessionId::new("side"));
        let stream = open(&path).unwrap();
        stream.emit(user("a").session(main.clone())).unwrap();
        // One commit holding events of both sessions and of none.
        stream
            .commit([
                user("b").session(side.clone()),
                user("c").session(main),
                Draft::new(Source::Host, "plugin/activated"),
            ])
            .unwrap();
        stream.emit(user("d").session(side.clone())).unwrap();
        drop(stream);

        let removed = remove(&path, |events| {
            assert_eq!(events.len(), 5);
            Ok(markers(&["side"]))
        })
        .unwrap();

        assert_eq!(removed, BTreeSet::from([side.clone()]));
        let stream = open(&path).unwrap();
        let kept: Vec<(u64, String)> = stream
            .events()
            .into_iter()
            .map(|event| (event.id.get(), event.topic))
            .collect();
        // What was left of each commit, then the marker after the last id the log held.
        let topics = ["a", "c", "plugin/activated", "session/removed"];
        let expected: Vec<(u64, String)> = [1, 3, 4, 6]
            .into_iter()
            .zip(topics.map(str::to_owned))
            .collect();
        assert_eq!(kept, expected);
        assert_eq!(stream.session(&side).len(), 0);
        assert_eq!(stream.events()[3].payload, "side");
        assert_eq!(stream.emit(user("e")).unwrap().get(), 7);
        drop(stream);
        let text = fs::read_to_string(&path).unwrap();
        // The header, the two commits left (one of them shorter), the marker and `e`.
        assert_eq!(text.lines().count(), 5, "{text}");
        assert!(!beside(&path).unwrap().exists());
    }

    #[test]
    fn a_removal_that_fails_or_picks_nothing_leaves_the_log_as_it_was() {
        let path = log_in("jsonl-remove-nothing");
        let stream = open(&path).unwrap();
        stream
            .emit(user("a").session(SessionId::new("main")))
            .unwrap();
        drop(stream);
        let before = fs::read_to_string(&path).unwrap();

        let none = remove(&path, |_| Ok(markers(&[]))).unwrap();
        let error = remove(&path, |_| Err(LogError::new("no such session"))).unwrap_err();
        let held = open(&path).unwrap();
        let locked = remove(&path, |_| Ok(markers(&["main"]))).unwrap_err();
        drop(held);

        assert_eq!(none.len(), 0);
        assert_eq!(error.to_string(), "event log: no such session");
        assert!(locked.to_string().contains("cannot lock"), "{locked}");
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn files_that_are_not_a_readable_log_are_refused_untouched() {
        let cases = [
            ("notes", "shopping list", "not a corduroy log"),
            ("foreign", "{\"other\":1}\n", "not a corduroy log"),
            ("future", "{\"corduroy-log\":3}\n", "schema version 3"),
            ("corrupt", "{\"corduroy-log\":1}\nnot json\n", "line 2 of"),
            (
                "backwards",
                "{\"corduroy-log\":2}\n[{\"id\":2,\"source\":\"user\",\"topic\":\"a\",\"payload\":\"\"}]\n\
                 [{\"id\":2,\"source\":\"user\",\"topic\":\"a\",\"payload\":\"\"}]\n",
                "does not follow #2",
            ),
        ];
        for (name, contents, expected) in cases {
            let path = log_in(&format!("jsonl-{name}"));
            fs::write(&path, contents).unwrap();

            let error = open(&path).unwrap_err().to_string();

            assert!(error.contains(expected), "{name}: {error}");
            assert_eq!(fs::read_to_string(&path).unwrap(), contents, "{name}");
        }
    }
}
