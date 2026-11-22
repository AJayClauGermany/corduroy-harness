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

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::Path;

use corduroy_core::{Event, EventId, EventStream, Log, LogError, SessionId};
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
    Ok(EventStream::replay(events).with_log(log))
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
        let path = path.as_ref();
        let fail = |what: &str, error: &dyn fmt::Display| {
            LogError::new(format!("{what} {}: {error}", path.display()))
        };
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(path)
            .map_err(|error| fail("cannot open", &error))?;
        file.try_lock()
            .map_err(|error| fail("cannot lock", &error))?;
        let bytes = read_locked(&mut file).map_err(|error| fail("cannot read", &error))?;

        // Everything after the last newline is a write that never finished. It may end in
        // the middle of a character, so it is set aside before the rest is read as text.
        let kept = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |newline| newline + 1);
        let (whole, torn) = bytes.split_at_checked(kept).unwrap_or((&[], &bytes));
        let whole = str::from_utf8(whole).map_err(|error| fail("cannot read", &error))?;
        let header = format!("{}", json!({HEADER_KEY: SCHEMA_VERSION}));
        let mut lines = whole.lines();
        let Some(first) = lines.next() else {
            if !header.as_bytes().starts_with(torn) {
                return Err(fail("cannot use", &"it is not a corduroy log"));
            }
            file.set_len(0)
                .and_then(|()| file.write_all(format!("{header}\n").as_bytes()))
                .and_then(|()| file.sync_data())
                .map_err(|error| fail("cannot start", &error))?;
            return Ok((Self { file, last: 0 }, Vec::new()));
        };
        check_header(first).map_err(|error| fail("cannot use", &error))?;

        let mut events: Vec<Event> = Vec::new();
        for (number, line) in lines.enumerate() {
            let last = events.last().map_or(0, |event| event.id.get());
            let commit = parse_commit(line, last)
                .map_err(|error| fail(&format!("line {} of", number + 2), &error))?;
            events.extend(commit);
        }
        if !torn.is_empty() {
            file.set_len(kept as u64)
                .and_then(|()| file.sync_data())
                .map_err(|error| fail("cannot repair", &error))?;
        }
        let last = events.last().map_or(0, |event| event.id.get());
        Ok((Self { file, last }, events))
    }
}

impl Log for JsonlLog {
    fn commit(&mut self, batch: &[Event]) -> Result<(), LogError> {
        let mut last = self.last;
        let mut line = Vec::with_capacity(batch.len());
        for event in batch {
            if event.id.get() != last + 1 {
                return Err(LogError::new(format!(
                    "event {} does not follow {last}, the last one stored",
                    event.id
                )));
            }
            last += 1;
            line.push(json!({
                "id": event.id.get(),
                "time": event.time,
                "session": event.session.as_ref().map(SessionId::as_str),
                "source": event.source.to_string(),
                "cause": event.cause.map(EventId::get),
                "topic": event.topic,
                "payload": event.payload,
            }));
        }
        // One write for the whole commit, so a crash cannot store only some of its events.
        let line = format!("{}\n", Value::Array(line));
        self.file
            .write_all(line.as_bytes())
            .and_then(|()| self.file.sync_data())
            .map_err(|error| LogError::new(format!("cannot store a commit: {error}")))?;
        self.last = last;
        Ok(())
    }
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

/// Decode one line into the events of a commit, which must continue from the id `last`.
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
        if id != last + 1 {
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
    use std::fs;
    use std::path::PathBuf;

    use corduroy_core::{Draft, Source};
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
        assert_eq!(fs::read_to_string(&path).unwrap(), whole);
        assert_eq!(stream.emit(user("next")).unwrap().get(), 2);
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

    #[test]
    fn files_that_are_not_a_readable_log_are_refused_untouched() {
        let cases = [
            ("notes", "shopping list", "not a corduroy log"),
            ("foreign", "{\"other\":1}\n", "not a corduroy log"),
            ("future", "{\"corduroy-log\":3}\n", "schema version 3"),
            ("corrupt", "{\"corduroy-log\":1}\nnot json\n", "line 2 of"),
            (
                "gap",
                "{\"corduroy-log\":1}\n[{\"id\":2,\"source\":\"user\",\"topic\":\"a\",\"payload\":\"\"}]\n",
                "does not follow #0",
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
