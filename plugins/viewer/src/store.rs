//! The files the page reads: the events in chunks, and an index of the chunks. Pure, so it
//! is tested natively.
//!
//! A page opened from a file cannot fetch, but it can load a script. So each file is a
//! script that hands its data to a function the page defines: a chunk calls
//! `corduroyChunk(number, [events])` and the index calls `corduroyIndex({...})`. A chunk is
//! one event to a line, so the one still being filled can be read back and added to.

use std::collections::BTreeMap;

use corduroy_guest::Event;
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

/// Events in one chunk, at most.
pub const MAX_EVENTS: usize = 200;
/// Bytes in one chunk, roughly at most.
pub const MAX_BYTES: usize = 512 * 1024;
/// A payload longer than this is cut, and says so: the log has it whole.
pub const MAX_PAYLOAD: usize = 256 * 1024;
/// Characters kept of a payload that is cut.
const KEPT: usize = 4000;
/// The format of these files. A page built for another one reads nothing.
pub const FORMAT: u32 = 1;

/// What the index says: which chunks there are, and how far they go.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    /// The format of the files, [`FORMAT`].
    pub format: u32,
    /// Changes whenever the chunks were written again from the start, so that a page drops
    /// what it had read.
    pub generation: u64,
    /// How many chunks there are, the last of them still being filled.
    pub chunks: usize,
    /// The id of the last event in them.
    pub last: u64,
    /// How many events are in them.
    pub events: u64,
    /// Whether the log holds more than has been written yet.
    pub behind: bool,
    /// Whether the person has been told where the page is.
    #[serde(default)]
    pub announced: bool,
    /// The sessions the chunks hold events of, each with the id of its last one. A session
    /// is removed from the log whole, so if that event is no longer there, neither is the
    /// session.
    #[serde(default)]
    pub sessions: BTreeMap<String, u64>,
}

impl Index {
    /// The index as the script the page loads.
    pub fn script(&self) -> String {
        let json = serde_json::to_string(self).unwrap_or_else(|_| "{}".to_owned());
        format!("corduroyIndex({json});\n")
    }

    /// The index a script written by [`script`](Self::script) holds, if `script` is one of
    /// this format.
    pub fn read(script: &str) -> Option<Self> {
        let json = script
            .trim()
            .strip_prefix("corduroyIndex(")?
            .strip_suffix(");")?;
        serde_json::from_str::<Self>(json)
            .ok()
            .filter(|index| index.format == FORMAT)
    }
}

/// The name of the file chunk `number` is kept in.
pub fn chunk_name(number: usize) -> String {
    format!("events-{number:06}.js")
}

/// Chunk `number` as the script the page loads, from its events' lines.
pub fn chunk_script(number: usize, lines: &[String]) -> String {
    let mut script = format!("corduroyChunk({number},[\n");
    for line in lines {
        script.push_str(line);
        script.push_str(",\n");
    }
    script.push_str("]);\n");
    script
}

/// The events' lines of a script written by [`chunk_script`] for chunk `number`, if
/// `script` is that.
pub fn chunk_lines(number: usize, script: &str) -> Option<Vec<String>> {
    let body = script
        .strip_prefix(&format!("corduroyChunk({number},[\n"))?
        .strip_suffix("]);\n")?;
    body.lines()
        .map(|line| line.strip_suffix(',').map(str::to_owned))
        .collect()
}

/// `event` as one line of a chunk: a JSON object with the event's fields, and its payload
/// as the JSON it is (or as text, where it is not JSON). A payload over [`MAX_PAYLOAD`] is
/// its first characters as text, with how many bytes it was under `cut`.
pub fn line(event: &Event) -> String {
    let text = |text: &str| serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned());
    let (payload, cut) = if event.payload.len() > MAX_PAYLOAD {
        let start: String = event.payload.chars().take(KEPT).collect();
        (text(&start), format!(",\"cut\":{}", event.payload.len()))
    } else if is_json(&event.payload) {
        (event.payload.clone(), String::new())
    } else {
        (text(&event.payload), String::new())
    };
    let session = event
        .session
        .as_deref()
        .map_or_else(|| "null".to_owned(), text);
    let cause = event
        .cause
        .map_or_else(|| "null".to_owned(), |cause| cause.to_string());
    format!(
        "{{\"id\":{},\"time\":{},\"session\":{session},\"source\":{},\"cause\":{cause},\
         \"topic\":{},\"payload\":{payload}{cut}}}",
        event.id,
        event.time,
        text(&event.source),
        text(&event.topic),
    )
}

/// Whether `text` is one JSON value on one line, so that it can be written into a chunk
/// as it is.
fn is_json(text: &str) -> bool {
    !text.contains('\n') && serde_json::from_str::<IgnoredAny>(text).is_ok()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn event(id: u64, topic: &str, payload: &str) -> Event {
        Event {
            id,
            time: 1_795_701_802_000,
            session: Some("main".to_owned()),
            source: "agent".to_owned(),
            cause: Some(id - 1),
            topic: topic.to_owned(),
            payload: payload.to_owned(),
        }
    }

    #[test]
    fn an_event_is_one_line_of_json_with_its_payload_as_json() {
        let written = line(&event(
            7,
            "tool/call",
            r#"{"tool":"bash","input":{"command":"ls"}}"#,
        ));
        assert!(!written.contains('\n'));
        let read: Value = serde_json::from_str(&written).unwrap();
        assert_eq!(
            read,
            json!({
                "id": 7, "time": 1_795_701_802_000_u64, "session": "main", "source": "agent",
                "cause": 6, "topic": "tool/call",
                "payload": {"tool": "bash", "input": {"command": "ls"}},
            })
        );
    }

    #[test]
    fn a_payload_that_is_not_json_is_text_and_a_huge_one_is_cut() {
        let mut harness = event(3, "session/removed", "side \"quest\"\n");
        harness.session = None;
        harness.cause = None;
        let read: Value = serde_json::from_str(&line(&harness)).unwrap();
        assert_eq!(read["payload"], json!("side \"quest\"\n"));
        assert_eq!(read["session"], Value::Null);
        assert_eq!(read["cause"], Value::Null);
        // JSON on several lines would break the one-line rule: it is kept as text.
        let spread = line(&event(4, "x", "{\n\"a\": 1}"));
        assert!(!spread.contains('\n'));
        assert_eq!(
            serde_json::from_str::<Value>(&spread).unwrap()["payload"],
            json!("{\n\"a\": 1}")
        );
        let huge = format!("{{\"output\":\"{}\"}}", "é".repeat(MAX_PAYLOAD));
        let read: Value = serde_json::from_str(&line(&event(5, "tool/result", &huge))).unwrap();
        assert_eq!(read["cut"], json!(huge.len()));
        assert_eq!(read["payload"].as_str().unwrap().chars().count(), KEPT);
    }

    #[test]
    fn a_chunk_is_read_back_as_the_lines_it_was_written_from() {
        let lines = vec![
            line(&event(
                1,
                "message",
                r#"{"role":"user","parts":[{"type":"text","text":"a,\nb"}]}"#,
            )),
            line(&event(2, "turn/end", r#"{"status":"finished"}"#)),
        ];
        let script = chunk_script(3, &lines);
        assert!(script.starts_with("corduroyChunk(3,[\n{\"id\":1,"));
        assert_eq!(chunk_lines(3, &script).unwrap(), lines);
        assert_eq!(
            chunk_lines(3, &chunk_script(3, &[])).unwrap(),
            Vec::<String>::new()
        );
        // Another chunk's file, or one cut short, is not read.
        assert_eq!(chunk_lines(4, &script), None);
        assert_eq!(chunk_lines(3, script.trim_end_matches("]);\n")), None);
        assert_eq!(chunk_name(3), "events-000003.js");
    }

    #[test]
    fn the_index_is_read_back_unless_it_is_of_another_format() {
        let index = Index {
            format: FORMAT,
            generation: 2,
            chunks: 3,
            last: 412,
            events: 400,
            behind: false,
            announced: true,
            sessions: BTreeMap::from([("main".to_owned(), 412), ("main/1".to_owned(), 380)]),
        };
        assert_eq!(Index::read(&index.script()).unwrap(), index);
        let other = Index {
            format: FORMAT + 1,
            ..index
        };
        assert_eq!(Index::read(&other.script()), None);
        assert_eq!(Index::read("corduroyIndex({);"), None);
        // One written before the page was announced says nothing of it.
        let earlier = r#"corduroyIndex({"format":1,"generation":1,"chunks":0,"last":0,"events":0,"behind":false});"#;
        let earlier = Index::read(earlier).unwrap();
        assert!(!earlier.announced && earlier.sessions.is_empty());
        assert_eq!(Index::read(""), None);
    }
}
