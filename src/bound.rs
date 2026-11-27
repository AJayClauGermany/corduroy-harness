//! How much a step's tool results may add to what the model is sent.
//!
//! A tool says how large its results should be at most and which part of a longer one is
//! kept ([`ResultBudget`] on its [`ToolSpec`](corduroy_core::ToolSpec)); the harness holds
//! each call to that, and to less where the model holds little, and holds the results of one
//! step together to a share of the model's context. Sizes are in tokens counted as
//! [`BYTES_PER_TOKEN`] bytes of text, since only the model's own tokenizer knows better.
//!
//! | Bound | Size | Applies |
//! | --- | --- | --- |
//! | One call | the tool's own budget, and at most an eighth of the model's context | when the tool or the model says a size |
//! | One step | a quarter of the model's context, shared out so that small results are kept whole | when the model says what it holds |
//!
//! A tool that asks ([`call::budget`](corduroy_core::call::budget)) is told its call's bound
//! and can choose what to leave out; whatever is still longer is cut here, by the tool's own
//! [`Keep`], and says so in its text. The cut is made to what the model is shown, the
//! `message` that carries the results: each `tool/result` holds what its tool returned, so
//! the log keeps the whole of it and the conversation is still rebuilt from `message` events
//! alone. Images are not counted.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use corduroy_core::{
    BYTES_PER_TOKEN, Content, Keep, Message, ModelInfo, Part, ResultBudget, ToolOutput,
};

/// The share of the model's context one call's result may take: one part in this many.
const CALL_SHARE: u64 = 8;
/// The share of the model's context one step's results may take together: one part in this
/// many.
const STEP_SHARE: u64 = 4;
/// Bytes set aside for the line that says a result was cut.
const NOTE: usize = 160;

/// The bounds on tool results for a model that holds a known number of tokens, or none.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bounds {
    context: Option<u64>,
}

impl Bounds {
    /// The bounds for the model `info` describes.
    pub(crate) const fn for_model(info: &ModelInfo) -> Self {
        Self {
            context: info.context_tokens,
        }
    }

    /// What one call of a tool that declared `declared` may return: the tool's own budget,
    /// held to the model's share. `None` if neither says a size.
    pub(crate) fn call(&self, declared: Option<ResultBudget>) -> Option<ResultBudget> {
        let share = self
            .context
            .map(|context| u32::try_from(context / CALL_SHARE).unwrap_or(u32::MAX));
        match (declared, share) {
            (Some(declared), Some(share)) => {
                Some(ResultBudget::new(declared.tokens.min(share), declared.keep))
            }
            (Some(declared), None) => Some(declared),
            (None, Some(share)) => Some(ResultBudget::new(share, Keep::default())),
            (None, None) => None,
        }
    }

    /// What one step's results may take together, in bytes of text.
    fn step(&self) -> Option<usize> {
        let tokens = self.context? / STEP_SHARE;
        Some(
            usize::try_from(tokens.saturating_mul(u64::from(BYTES_PER_TOKEN)))
                .unwrap_or(usize::MAX),
        )
    }

    /// `results`, the message that carries a step's tool results to the model, with each
    /// result held to its call's bound in `calls` (by tool-call id) and all of them
    /// together to the step's.
    pub(crate) fn hold(
        &self,
        mut results: Message,
        calls: &BTreeMap<String, ResultBudget>,
    ) -> Message {
        // What each result would take once held to its own call's bound.
        let room = |id: &str| calls.get(id).map(|budget| budget.bytes());
        let sizes: Vec<usize> = results
            .tool_results()
            .map(|result| {
                let size = text_bytes(&result.output);
                room(&result.id).map_or(size, |room| size.min(room))
            })
            .collect();
        // And what each may keep so that the step's results fit together. A result is cut
        // once, to the smaller of the two, so its note counts from what the tool returned.
        let level = self.step().and_then(|step| level(&sizes, step));
        for part in &mut results.parts {
            let Part::ToolResult(result) = part else {
                continue;
            };
            let bytes = match (room(&result.id), level) {
                (Some(room), Some(level)) => room.min(level),
                (Some(bytes), None) | (None, Some(bytes)) => bytes,
                (None, None) => continue,
            };
            let keep = calls
                .get(&result.id)
                .map_or_else(Keep::default, |budget| budget.keep);
            if let Some(cut) = fit(&result.output, bytes, keep) {
                result.output = cut;
            }
        }
        results
    }
}

/// The most each of results of `sizes` bytes may keep so that together they come to `room`
/// bytes or less, the small ones whole. `None` if they already do.
fn level(sizes: &[usize], room: usize) -> Option<usize> {
    if sizes.iter().sum::<usize>() <= room {
        return None;
    }
    let mut sorted = sizes.to_vec();
    sorted.sort_unstable();
    let (mut left, mut room) = (sorted.len(), room);
    for size in sorted {
        let each = room.checked_div(left).unwrap_or(0);
        if size > each {
            return Some(each);
        }
        room = room.saturating_sub(size);
        left = left.saturating_sub(1);
    }
    None
}

/// The bytes of text in `output`.
fn text_bytes(output: &ToolOutput) -> usize {
    output
        .content()
        .iter()
        .map(|block| match block {
            Content::Text(text) => text.len(),
            _ => 0,
        })
        .sum()
}

/// `output` with its text cut to `bytes`, keeping `keep` of each block and saying what was
/// left out. `None` if it fits as it is.
fn fit(output: &ToolOutput, bytes: usize, keep: Keep) -> Option<ToolOutput> {
    let total = text_bytes(output);
    if total <= bytes {
        return None;
    }
    let blocks = output.content().iter().map(|block| match block {
        Content::Text(text) => {
            // Each block of text gets the share of the room that it has of the text.
            let share = u128::try_from(bytes)
                .unwrap_or(u128::MAX)
                .saturating_mul(u128::try_from(text.len()).unwrap_or(u128::MAX))
                .checked_div(u128::try_from(total).unwrap_or(u128::MAX))
                .unwrap_or(0);
            Content::text(kept(text, usize::try_from(share).unwrap_or(bytes), keep))
        }
        other => other.clone(),
    });
    Some(blocks.collect())
}

/// `text` cut to about `bytes`, keeping the part `keep` names and saying in a line what was
/// left out. Text that fits is returned as it is.
pub(crate) fn kept(text: &str, bytes: usize, keep: Keep) -> String {
    if text.len() <= bytes {
        return String::from(text);
    }
    let total = text.len();
    // With little room the note is a few words, and half the room is the text's.
    if bytes <= NOTE.saturating_mul(2) {
        let shown = match keep {
            Keep::Tail => tail(text, bytes / 2),
            _ => head(text, bytes / 2),
        };
        let which = if keep == Keep::Tail { "last" } else { "first" };
        return format!(
            "{shown}\n[cut: the {which} {} of {total} bytes]",
            shown.len()
        );
    }
    let room = bytes.saturating_sub(NOTE);
    match keep {
        Keep::Head => {
            let head = head(text, room);
            format!(
                "{head}\n[cut: the first {} of {total} bytes are shown ({} lines), which is \
                 what one result may take of this model's context. Ask for the rest in parts.]",
                head.len(),
                head.lines().count(),
            )
        }
        Keep::Tail => {
            let tail = tail(text, room);
            format!(
                "[cut: the last {} of {total} bytes are shown, which is what one result may \
                 take of this model's context. Ask for less at once to see the rest.]\n{tail}",
                tail.len(),
            )
        }
        _ => {
            let head = head(text, room / 2);
            let tail = tail(text, room.saturating_sub(head.len()));
            let dropped = total.saturating_sub(head.len()).saturating_sub(tail.len());
            format!(
                "{head}\n[cut: {dropped} of {total} bytes are left out here, to keep to what \
                 one result may take of this model's context. Ask for less at once to see \
                 them.]\n{tail}"
            )
        }
    }
}

/// The start of `text`, at most `bytes` long, ending at the end of a line if one ends in
/// its last quarter.
fn head(text: &str, bytes: usize) -> &str {
    let mut end = bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    let head = text.get(..end).unwrap_or_default();
    match head.rfind('\n') {
        Some(line) if line >= end.saturating_sub(end / 4) => head.get(..line).unwrap_or(head),
        _ => head,
    }
}

/// The end of `text`, at most `bytes` long, starting at the start of a line if one starts in
/// its first quarter.
fn tail(text: &str, bytes: usize) -> &str {
    let mut start = text.len().saturating_sub(bytes);
    while !text.is_char_boundary(start) {
        start = start.saturating_add(1);
    }
    let tail = text.get(start..).unwrap_or_default();
    match tail.find('\n') {
        Some(line) if line <= tail.len() / 4 => tail.get(line.saturating_add(1)..).unwrap_or(tail),
        _ => tail,
    }
}

#[cfg(test)]
mod tests {
    use alloc::borrow::ToOwned;

    use corduroy_core::ToolResult;

    use super::*;

    fn lines(count: usize) -> String {
        (1..=count)
            .map(|line| format!("line {line:04}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn each_way_of_cutting_keeps_its_part_and_says_what_is_gone() {
        let text = lines(1000);
        let head = kept(&text, 2000, Keep::Head);
        assert!(head.starts_with("line 0001\n"), "{head}");
        assert!(head.contains("[cut: the first "), "{head}");
        assert!(!head.contains("line 1000"));
        assert!(head.len() <= 2100, "{}", head.len());

        let tail = kept(&text, 2000, Keep::Tail);
        assert!(tail.starts_with("[cut: the last "), "{tail}");
        assert!(tail.ends_with("line 1000"), "{tail}");
        assert!(!tail.contains("line 0001"));

        let ends = kept(&text, 2000, Keep::Ends);
        assert!(ends.starts_with("line 0001\n"), "{ends}");
        assert!(ends.ends_with("line 1000"), "{ends}");
        assert!(ends.contains("bytes are left out here"), "{ends}");

        // Whole lines are kept where one ends near the cut, and text that fits is left be.
        assert!(
            head.lines()
                .all(|line| line.len() == 9 || line.starts_with("[cut"))
        );
        assert_eq!(kept("short", 2000, Keep::Head), "short");
    }

    #[test]
    fn a_cut_never_parts_a_character() {
        let text = "é".repeat(500);
        for keep in [Keep::Head, Keep::Tail, Keep::Ends] {
            let cut = kept(&text, 101, keep);
            assert!(cut.len() < text.len().saturating_add(NOTE.saturating_mul(2)));
            assert!(cut.contains("[cut"));
        }
    }

    #[test]
    fn a_call_is_held_to_its_tools_budget_and_to_the_models_share() {
        let declared = Some(ResultBudget::new(16_384, Keep::Tail));
        let small = Bounds::for_model(&ModelInfo::unknown().with_context(16_384));
        assert_eq!(
            small.call(declared),
            Some(ResultBudget::new(2048, Keep::Tail))
        );
        assert_eq!(small.call(None), Some(ResultBudget::new(2048, Keep::Ends)));
        let large = Bounds::for_model(&ModelInfo::unknown().with_context(1_000_000));
        assert_eq!(large.call(declared), declared);
        let unknown = Bounds::for_model(&ModelInfo::unknown());
        assert_eq!(unknown.call(declared), declared);
        assert_eq!(unknown.call(None), None);
    }

    #[test]
    fn a_step_shares_its_room_and_keeps_small_results_whole() {
        // A model of 1200 tokens: a step may add 300 of them, 900 bytes.
        let bounds = Bounds::for_model(&ModelInfo::unknown().with_context(1200));
        let mut results = Message::user();
        for (id, size) in [("a", 100), ("b", 2000), ("c", 2000)] {
            results = results.result(ToolResult::new(id, true, "x".repeat(size)));
        }
        let held = bounds.hold(results, &BTreeMap::new());
        let sizes: Vec<usize> = held
            .tool_results()
            .map(|result| result.output.to_text().len())
            .collect();
        assert_eq!(sizes.first(), Some(&100));
        assert!(sizes.iter().sum::<usize>() <= 900, "{sizes:?}");
        assert!(sizes.get(1).is_some_and(|size| *size > 300), "{sizes:?}");
        assert!(
            held.tool_results().nth(1).is_some_and(|result| {
                result.output.to_text().contains("bytes are left out here")
            })
        );
        assert_eq!(level(&[100, 100], 900), None);
        assert_eq!(level(&[100, 2000, 2000], 900), Some(400));
    }

    #[test]
    fn a_result_within_its_bounds_is_left_as_it_was() {
        let bounds = Bounds::for_model(&ModelInfo::unknown());
        let results = Message::user().result(ToolResult::new("a", true, "x".repeat(100_000)));
        assert_eq!(bounds.hold(results.clone(), &BTreeMap::new()), results);
        let calls = BTreeMap::from([("a".to_owned(), ResultBudget::new(100, Keep::Head))]);
        let held = bounds.hold(results, &calls);
        let text = held
            .tool_results()
            .map(|result| result.output.to_text())
            .collect::<Vec<_>>();
        assert!(
            text.first().is_some_and(|text| text.len() <= 300 + NOTE),
            "{text:?}"
        );
    }
}
