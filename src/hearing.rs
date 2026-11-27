//! Hearing a provider while it answers a request.
//!
//! Retries are recorded, and the pieces of the answer go to the [live channel](crate::live)
//! and, at a coarse grain, into checkpoints in the log.
//!
//! A checkpoint (`model/progress`) holds what the model has written since the one before,
//! so a log read after a crash shows how far the answer got. They are written by time and
//! not by piece ([`Checkpoints`]). A request that ends without an answer (the run was
//! stopped, ran out of time, or the model failed) leaves what it had written, once and
//! whole, as `model/partial`. Neither is part of the conversation: the model is never shown
//! them and nothing in them is run.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;
use core::mem;
use core::time::Duration;
#[cfg(feature = "std")]
use std::time::Instant;

use corduroy_core::sync::Lock;
use corduroy_core::{BYTES_PER_TOKEN, Usage};
use serde_json::{Map, Value, json};

use crate::live::{Live, Seen};
use crate::provider::{CallPiece, Piece, ProviderErrorKind, Reports, Retry};
use crate::session::{Journal, MODEL_PARTIAL, MODEL_PROGRESS};

/// How often what a model is writing is checkpointed in the log while it writes.
///
/// A checkpoint is written when a piece arrives and the last checkpoint is at least this
/// old. The first piece of an answer starts the clock, so an answer that comes whole, or
/// within one interval, is never checkpointed: it is in the log as its `message` soon
/// enough.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Checkpoints {
    /// None: only the whole answer is recorded, and what a request that never answered had
    /// written.
    Off,
    /// At most one for each such interval; with no time at all, one for every piece.
    /// Without the `std` feature there is no clock, and one is written for every
    /// [`CHECKPOINT_TOKENS`] tokens instead.
    Every(Duration),
}

impl Default for Checkpoints {
    /// Every five seconds.
    fn default() -> Self {
        Self::Every(Duration::from_secs(5))
    }
}

/// How many tokens of an answer go into one checkpoint where there is no clock to go by.
pub const CHECKPOINT_TOKENS: u64 = 2048;

/// Why a request that never answered left what it had written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unanswered {
    /// A person stopped the run.
    Stopped,
    /// The run's time ran out.
    TimeLimit,
    /// The model failed, in this way.
    Failed(ProviderErrorKind),
}

/// Text, thinking and tool calls a model wrote.
#[derive(Debug, Default)]
struct Parts {
    text: String,
    thinking: String,
    calls: Vec<CallPiece>,
}

impl Parts {
    const fn is_empty(&self) -> bool {
        self.text.is_empty() && self.thinking.is_empty() && self.calls.is_empty()
    }

    /// Add `piece`. A piece of a call that is already here adds to it.
    fn add(&mut self, piece: &Piece) {
        match piece {
            Piece::Text(text) => self.text.push_str(text),
            Piece::Thinking(text) => self.thinking.push_str(text),
            Piece::Call(piece) => match self.calls.iter_mut().find(|call| call.id == piece.id) {
                Some(call) => {
                    if call.name.is_empty() {
                        call.name.clone_from(&piece.name);
                    }
                    call.arguments.push_str(&piece.arguments);
                }
                None => self.calls.push(piece.clone()),
            },
            _ => {}
        }
    }

    /// Put what is here into `payload`, each kind only if there is some.
    fn write(&self, payload: &mut Map<String, Value>) {
        if !self.text.is_empty() {
            payload.insert("text".to_owned(), json!(self.text));
        }
        if !self.thinking.is_empty() {
            payload.insert("thinking".to_owned(), json!(self.thinking));
        }
        if !self.calls.is_empty() {
            let calls: Vec<Value> = self
                .calls
                .iter()
                .map(|call| json!({"id": call.id, "tool": call.name, "input": call.arguments}))
                .collect();
            payload.insert("calls".to_owned(), Value::Array(calls));
        }
    }
}

/// What one attempt at a request has written so far.
#[derive(Debug, Default)]
struct Written {
    /// All of it.
    whole: Parts,
    /// What came since the last checkpoint.
    since: Parts,
    /// How many bytes of text all of it is.
    bytes: u64,
    /// What the provider said the request has cost so far, if it said.
    usage: Option<Usage>,
    /// When the last checkpoint was written, or the first piece came.
    #[cfg(feature = "std")]
    marked: Option<Instant>,
    /// How many tokens had been written at the last checkpoint.
    #[cfg(not(feature = "std"))]
    marked: u64,
}

impl Written {
    /// How many tokens were written, and whether that is an estimate from the length of
    /// the text and not the provider's count.
    fn tokens(&self) -> (u64, bool) {
        let said = self.usage.and_then(|usage| usage.output_tokens);
        let per_token = u64::from(BYTES_PER_TOKEN).max(1);
        said.map_or_else(
            || (self.bytes.div_ceil(per_token), true),
            |tokens| (tokens, false),
        )
    }

    /// Whether a checkpoint is due now, by `checkpoints`; if so, it is taken to be written.
    #[cfg(feature = "std")]
    fn due(&mut self, checkpoints: Checkpoints) -> bool {
        let Checkpoints::Every(interval) = checkpoints else {
            return false;
        };
        let now = Instant::now();
        // The first piece starts the clock.
        let since = *self.marked.get_or_insert(now);
        let due =
            !self.since.is_empty() && (interval.is_zero() || now.duration_since(since) >= interval);
        if due {
            self.marked = Some(now);
        }
        due
    }

    /// Whether a checkpoint is due now, by `checkpoints`; if so, it is taken to be written.
    #[cfg(not(feature = "std"))]
    fn due(&mut self, checkpoints: Checkpoints) -> bool {
        let Checkpoints::Every(interval) = checkpoints else {
            return false;
        };
        let (tokens, _) = self.tokens();
        let due = !self.since.is_empty()
            && (interval.is_zero() || tokens.saturating_sub(self.marked) >= CHECKPOINT_TOKENS);
        if due {
            self.marked = tokens;
        }
        due
    }
}

/// Records what a provider reports while it answers one request of a session, and shows
/// the pieces to whoever watches.
pub(crate) struct Heard<'run> {
    journal: &'run Journal,
    live: &'run Live,
    provider: &'run str,
    model: &'run str,
    checkpoints: Checkpoints,
    written: Lock<Written>,
    /// When the request went out.
    #[cfg(feature = "std")]
    began: Instant,
    /// The first report that could not be recorded.
    failed: Lock<Option<anyhow::Error>>,
}

impl<'run> Heard<'run> {
    /// Hear the request that is about to go to `model` of `provider` for `journal`'s
    /// session.
    pub(crate) fn new(
        journal: &'run Journal,
        live: &'run Live,
        provider: &'run str,
        model: &'run str,
        checkpoints: Checkpoints,
    ) -> Self {
        Self {
            journal,
            live,
            provider,
            model,
            checkpoints,
            written: Lock::default(),
            #[cfg(feature = "std")]
            began: Instant::now(),
            failed: Lock::new(None),
        }
    }

    /// The first report that could not be recorded, if one could not: the stream is closed
    /// then, which has to reach whoever drives the run.
    pub(crate) fn failed(&self) -> Option<anyhow::Error> {
        self.failed.with(Option::take)
    }

    /// Record what the request had written, as it ends without an answer for `why`.
    /// Nothing is recorded if it had written nothing.
    pub(crate) fn unanswered(&self, why: Unanswered) -> anyhow::Result<()> {
        let payload = self.written.with(|written| {
            if written.whole.is_empty() {
                return None;
            }
            let mut payload = self.payload(written);
            let (why, kind) = match why {
                Unanswered::Stopped => ("stopped", None),
                Unanswered::TimeLimit => ("time-limit", None),
                Unanswered::Failed(kind) => ("failed", Some(kind.name())),
            };
            payload.insert("why".to_owned(), json!(why));
            if let Some(kind) = kind {
                payload.insert("kind".to_owned(), json!(kind));
            }
            written.whole.write(&mut payload);
            Some(payload)
        });
        payload.map_or(Ok(()), |payload| {
            self.journal.noted(MODEL_PARTIAL, &Value::Object(payload))
        })
    }

    /// What every record of this request says: whose it is, how much was written and for
    /// how long.
    fn payload(&self, written: &Written) -> Map<String, Value> {
        let mut payload = Map::new();
        payload.insert("provider".to_owned(), json!(self.provider));
        payload.insert("model".to_owned(), json!(self.model));
        let (tokens, estimated) = written.tokens();
        payload.insert("tokens".to_owned(), json!(tokens));
        if estimated {
            payload.insert("estimated".to_owned(), json!(true));
        }
        #[cfg(feature = "std")]
        payload.insert(
            "elapsed_ms".to_owned(),
            json!(u64::try_from(self.began.elapsed().as_millis()).unwrap_or(u64::MAX)),
        );
        payload
    }

    /// Keep the first error a record met.
    fn keep(&self, recorded: anyhow::Result<()>) {
        if let Err(error) = recorded {
            self.failed.with(|failed| {
                failed.get_or_insert(error);
            });
        }
    }
}

impl Reports for Heard<'_> {
    fn retry(&self, retry: &Retry) {
        // What the failed attempt wrote is void: the next one starts its answer again.
        self.written.with(mem::take);
        self.live.tell(self.journal.id(), &Seen::Restarted);
        self.keep(self.journal.retried(self.provider, self.model, retry));
    }

    fn piece(&self, piece: &Piece) {
        self.live
            .tell(self.journal.id(), &Seen::Piece(piece.clone()));
        let checkpoint = self.written.with(|written| {
            match piece {
                Piece::Text(text) | Piece::Thinking(text) => {
                    written.bytes = written.bytes.saturating_add(len(text));
                }
                Piece::Call(call) => {
                    written.bytes = written.bytes.saturating_add(len(&call.arguments));
                }
                Piece::Usage(usage) => written.usage = Some(*usage),
            }
            written.whole.add(piece);
            written.since.add(piece);
            if !written.due(self.checkpoints) {
                return None;
            }
            let mut payload = self.payload(written);
            mem::take(&mut written.since).write(&mut payload);
            Some(payload)
        });
        if let Some(payload) = checkpoint {
            self.keep(self.journal.noted(MODEL_PROGRESS, &Value::Object(payload)));
        }
    }
}

/// The length of `text`, as a count that cannot overflow.
fn len(text: &str) -> u64 {
    u64::try_from(text.len()).unwrap_or(u64::MAX)
}
