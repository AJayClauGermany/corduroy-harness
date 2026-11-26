//! The event log as a page in a browser: what each session said and did, how long each
//! part took, what it cost in tokens, and what went wrong.
//!
//! Everything an agent does is on the event stream, and `corduroy log` prints it a line an
//! event. This plugin writes the same record as a page: it reads the stream through
//! `log-read`, and writes the page and the events through `fs-write` into one directory
//! (`<dir>/index.html`), which you open in a browser. While a run goes the page follows
//! it.
//!
//! An add-on cannot listen on a port, so nothing is served: the page is files, opened as
//! files. It shows and never sends: it holds no way to answer a gate or type a prompt,
//! which stay in the terminal. A page that is served, and that can be talked through, is
//! a front end, and a front end is an extension.
//!
//! What it writes is a copy of the log, so whatever can read the directory can read
//! what was said. A session removed from the log (`corduroy sessions remove`), or one the
//! grant no longer reaches, leaves the page the next time the agent starts: the copy is
//! then written again without it.
//!
//! Configuration: `dir`, where the page is written (`viewer`, in the workspace). Needs
//! `log-read`, `fs-read` and `fs-write` on `dir`, and `log`.

mod store;

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use corduroy_guest::host::log::Level;
use corduroy_guest::host::{fs_read, fs_write, log_read};
use corduroy_guest::{Event, HostError, Plugin, PluginInfo, log};
use serde::Deserialize;

use store::Index;

/// The page, and the two files it loads.
const FILES: [(&str, &str); 3] = [
    ("index.html", include_str!("../page/index.html")),
    ("app.js", include_str!("../page/app.js")),
    ("style.css", include_str!("../page/style.css")),
];
/// The file the index is kept in.
const INDEX: &str = "index.js";
/// Events read from the log at a time (the host's limit).
const PAGE: u32 = 1000;
/// Events written by one call into the plugin, at most: a call is metered, so a long log
/// is caught up with over several.
const MAX_PER_CALL: usize = 20_000;
/// The events after which the page is brought up to date.
const FOLLOWED: [&str; 11] = [
    "session/start",
    "message",
    "tool/call",
    "gate/decision",
    "gate/answer",
    "tool/result",
    "tool/wait",
    "run/stop",
    "turn/end",
    "plugin/failed",
    "plugin/stopped",
];

/// Plugin configuration, from the host.
#[derive(Deserialize)]
#[serde(default)]
struct Config {
    /// The directory the page is written to.
    dir: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dir: "viewer".to_owned(),
        }
    }
}

/// How far the writer has got.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// What an earlier process wrote has not been looked at yet.
    Unread,
    /// The files follow the log.
    Following,
    /// A file could not be written: nothing more is tried, so that a directory out of
    /// reach is said once and not at every event.
    Broken,
}

/// What has been written, and what is still being filled.
struct Writer {
    dir: String,
    stage: Stage,
    index: Index,
    /// Chunks that are full, and so never written again.
    sealed: usize,
    /// The lines of the chunk still being filled, and how many bytes they are.
    open: Vec<String>,
    open_bytes: usize,
    /// How many chunks an earlier writing left that this one is to remove once it has
    /// written its own: what they held is no longer in the log.
    leftover: usize,
}

static WRITER: Mutex<Writer> = Mutex::new(Writer::new());

impl Writer {
    const fn new() -> Self {
        Self {
            dir: String::new(),
            stage: Stage::Unread,
            index: Index {
                format: store::FORMAT,
                generation: 0,
                chunks: 0,
                last: 0,
                events: 0,
                behind: false,
                announced: false,
                sessions: BTreeMap::new(),
            },
            sealed: 0,
            open: Vec::new(),
            open_bytes: 0,
            leftover: 0,
        }
    }

    fn path(&self, name: &str) -> String {
        format!("{}/{name}", self.dir.trim_end_matches('/'))
    }

    /// Write the page, and take up where an earlier process left the events, if what it
    /// left can be read.
    fn resume(&mut self) -> Result<(), HostError> {
        for (name, contents) in FILES {
            fs_write::write(&self.path(name), contents)?;
        }
        let earlier = fs_read::read(&self.path(INDEX))
            .ok()
            .and_then(|script| Index::read(&script));
        let Some(index) = earlier else {
            self.start_over(1);
            return Ok(());
        };
        let last = index
            .chunks
            .checked_sub(1)
            .filter(|_| !lost_a_session(&index))
            .and_then(|number| {
                let script = fs_read::read(&self.path(&store::chunk_name(number))).ok()?;
                store::chunk_lines(number, &script)
            });
        match last {
            Some(lines) => {
                self.open_bytes = lines.iter().map(String::len).sum();
                self.open = lines;
                self.sealed = index.chunks.saturating_sub(1);
                self.index = index;
                if self.is_full() {
                    self.seal();
                }
            }
            None if index.chunks == 0 => self.index = index,
            // A session it holds is gone from the log, or its last chunk cannot be read:
            // nothing of it is kept.
            None => {
                self.start_over(index.generation + 1);
                self.index.announced = index.announced;
                self.leftover = index.chunks;
            }
        }
        Ok(())
    }

    /// Forget what was written, so that the next read of the log starts at its beginning.
    fn start_over(&mut self, generation: u64) {
        self.index = Index {
            format: store::FORMAT,
            generation,
            ..Index::default()
        };
        self.sealed = 0;
        self.open.clear();
        self.open_bytes = 0;
    }

    fn is_full(&self) -> bool {
        self.open.len() >= store::MAX_EVENTS || self.open_bytes >= store::MAX_BYTES
    }

    /// Count the chunk being filled as full: the next event begins another.
    fn seal(&mut self) {
        self.sealed += 1;
        self.open.clear();
        self.open_bytes = 0;
    }

    /// Write the chunk being filled, if it holds anything.
    fn write_open(&mut self) -> Result<(), HostError> {
        if self.open.is_empty() {
            return Ok(());
        }
        let script = store::chunk_script(self.sealed, &self.open);
        fs_write::write(&self.path(&store::chunk_name(self.sealed)), &script)?;
        self.index.chunks = self.sealed + 1;
        Ok(())
    }

    /// Add `event` to the chunk being filled, writing the chunk out when that fills it.
    fn add(&mut self, event: &Event) -> Result<(), HostError> {
        let line = store::line(event);
        self.open_bytes += line.len();
        self.open.push(line);
        self.index.last = event.id;
        self.index.events += 1;
        if let Some(session) = &event.session {
            self.index.sessions.insert(session.clone(), event.id);
        }
        if self.is_full() {
            self.write_open()?;
            self.seal();
        }
        Ok(())
    }

    /// Bring the files up to the end of the log, or as far as one call goes.
    fn sync(&mut self) -> Result<(), HostError> {
        if self.stage == Stage::Unread {
            self.resume()?;
            self.stage = Stage::Following;
        }
        let mut written = 0;
        loop {
            let events = log_read::read(None, "*", self.index.last, PAGE)?;
            for event in &events {
                self.add(event)?;
            }
            written += events.len();
            let more = events.len() == PAGE as usize;
            self.index.behind = more;
            if !more || written >= MAX_PER_CALL {
                break;
            }
        }
        if written > 0 {
            self.write_open()?;
        }
        // The index is written before what it no longer names is removed, so that a page
        // never reads a chunk that is going.
        fs_write::write(&self.path(INDEX), &self.index.script())?;
        for number in self.index.chunks.max(self.sealed)..self.leftover {
            // One that is already gone is as good as removed.
            drop(fs_write::remove(&self.path(&store::chunk_name(number))));
        }
        self.leftover = 0;
        Ok(())
    }
}

/// Whether a session the chunks hold is no longer in the log, or no longer within the
/// grant: its last event is not where it was. A session is removed whole (`corduroy
/// sessions remove`), so one event says it for all of them.
fn lost_a_session(index: &Index) -> bool {
    index.sessions.iter().any(|(session, last)| {
        let found = log_read::read(Some(session), "*", last.saturating_sub(1), 1);
        !found.is_ok_and(|events| events.first().is_some_and(|event| event.id == *last))
    })
}

/// Bring the page up to date, saying so once if it cannot be written. `in_a_run` says a
/// run is going, which is when a line in the log is shown to the person: the first time
/// the page is written, that is when they are told where it is.
fn follow(in_a_run: bool) {
    let mut writer = WRITER.lock().unwrap_or_else(PoisonError::into_inner);
    if writer.stage == Stage::Broken {
        return;
    }
    // Said before the files are written, so that the index written next remembers it.
    let announce = in_a_run && writer.stage == Stage::Following && !writer.index.announced;
    if announce {
        writer.index.announced = true;
        log(
            Level::Info,
            &format!(
                "open {} in a browser to see what the agent does",
                writer.path("index.html")
            ),
        );
    }
    if let Err(error) = writer.sync() {
        writer.stage = Stage::Broken;
        let (HostError::Denied(reason) | HostError::Failed(reason)) = error;
        log(
            Level::Warn,
            &format!("the page in {} is no longer written: {reason}", writer.dir),
        );
    }
}

struct Viewer;

impl Plugin for Viewer {
    fn activate(config: String) -> Result<PluginInfo, String> {
        let config: Config = corduroy_guest::config(&config)?;
        if config.dir.trim().is_empty() {
            return Err("bad config: `dir` is empty".to_owned());
        }
        {
            let mut writer = WRITER.lock().unwrap_or_else(PoisonError::into_inner);
            // A second activation in the same instance keeps the first configuration.
            if writer.dir.is_empty() {
                writer.dir = config.dir;
            }
        }
        // Whatever the last process wrote after its last followed event, and this one so
        // far, so that the page is current as soon as the agent has started.
        follow(false);
        let mut info = PluginInfo::new("viewer", env!("CARGO_PKG_VERSION"));
        for topic in FOLLOWED {
            info = info.subscribe(topic);
        }
        Ok(info)
    }

    fn on_event(event: Event) {
        follow(event.session.is_some());
    }
}

corduroy_guest::export!(Viewer);
