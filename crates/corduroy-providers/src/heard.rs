//! Hearing the retries oi-kit's own client makes.
//!
//! oi-kit's providers try a refused request again inside the call that sends it, by their
//! own policy, and offer no way to switch that off or to be told of it. The one sign is a
//! line each retry writes through `tracing`. `Heard` listens for that line while it polls
//! an attempt, and nowhere else, so that each such retry can be told to whoever asked and
//! recorded: a request is then made again by one layer only, and every time is known.
//!
//! Should oi-kit stop writing the line, nothing is heard, and the adapter's own retries
//! take over as if the client made none.

use std::cell::RefCell;
use std::fmt;
use std::pin::Pin;
use std::sync::OnceLock;
use std::task::{Context, Poll};
use std::time::Duration;

use corduroy_harness::{ProviderError, ProviderErrorKind};
use tracing_core::field::{Field, Visit};
use tracing_core::span::{Attributes, Id, Record};
use tracing_core::{Dispatch, Event, Level, LevelFilter, Metadata, Subscriber, dispatcher};

/// Where oi-kit's retry loop writes from.
const TARGET: &str = "oi_kit_provider_types::retry";

/// What oi-kit's line says before the error.
const SAID: &str = "retrying (";

thread_local! {
    /// The lines heard during the poll now under way on this thread, if one is listening.
    static LINES: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// Keeps oi-kit's retry lines, and nothing else, while a poll listens.
struct Listener;

impl Subscriber for Listener {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() == Level::WARN && metadata.target() == TARGET
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::WARN)
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut line = Line(String::new());
        event.record(&mut line);
        LINES.with_borrow_mut(|lines| {
            if let Some(lines) = lines {
                lines.push(line.0);
            }
        });
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

/// The message of one line.
struct Line(String);

impl Visit for Line {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

/// The one listener, made once: making a dispatcher has every call site asked again whether
/// anyone listens to it, which is what lets a line written later be heard at all.
fn listener() -> &'static Dispatch {
    static LISTENER: OnceLock<Dispatch> = OnceLock::new();
    LISTENER.get_or_init(|| Dispatch::new(Listener))
}

/// A retry oi-kit's client made by itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientRetry {
    /// Which retry of its own the client says this is, counting from 1.
    pub(crate) attempt: usize,
    /// Why the try before it failed, in oi-kit's words, and what sort of failure that is.
    pub(crate) error: ProviderError,
    /// How long the provider asked to be left alone, where the failure said.
    pub(crate) asked: Option<Duration>,
}

/// The retry `line` tells of, if it is oi-kit's line for one.
pub(crate) fn retry_in(line: &str) -> Option<ClientRetry> {
    let (_, rest) = line.split_once(SAID)?;
    let (count, error) = rest.split_once("): ")?;
    let attempt = count.split_once('/')?.0.parse().ok()?;
    let kind = if error.starts_with("RateLimitExceeded") {
        ProviderErrorKind::RateLimit
    } else if error.starts_with("NetworkError") {
        ProviderErrorKind::Unreachable
    } else {
        ProviderErrorKind::Other
    };
    Some(ClientRetry {
        attempt,
        error: ProviderError::new(error).of(kind),
        asked: asked_in(error),
    })
}

/// The wait a rate limit asked for, as oi-kit's error shows it (`retry_delay: Some(2s)`).
fn asked_in(error: &str) -> Option<Duration> {
    let (_, rest) = error.split_once("retry_delay: Some(")?;
    let shown = rest.split_once(')')?.0;
    let digits = shown.trim_end_matches(|unit: char| unit.is_alphabetic() || unit == 'µ');
    let unit = shown.strip_prefix(digits)?;
    let amount: f64 = digits.parse().ok()?;
    let seconds = match unit {
        "s" => amount,
        "ms" => amount / 1e3,
        "µs" | "us" => amount / 1e6,
        "ns" => amount / 1e9,
        _ => return None,
    };
    Duration::try_from_secs_f64(seconds).ok()
}

/// An attempt at a request, listened to: each retry oi-kit's client makes while the attempt
/// is polled is handed to `tell` as it happens, and the attempt's answer comes with how many
/// there were.
pub(crate) struct Heard<'attempt, T> {
    attempt: Pin<Box<dyn Future<Output = T> + Send + 'attempt>>,
    tell: Box<dyn FnMut(ClientRetry) + Send + 'attempt>,
    count: usize,
}

impl<'attempt, T> Heard<'attempt, T> {
    /// `attempt`, with each of the client's retries handed to `tell`.
    pub(crate) fn new(
        attempt: impl Future<Output = T> + Send + 'attempt,
        tell: impl FnMut(ClientRetry) + Send + 'attempt,
    ) -> Self {
        Self {
            attempt: Box::pin(attempt),
            tell: Box::new(tell),
            count: 0,
        }
    }
}

impl<T> Future for Heard<'_, T> {
    type Output = (T, usize);

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        // Whatever listened on this thread before (a poll of another request cannot be
        // under way on it, but a caller of ours may be listened to) hears again afterwards.
        let before = LINES.replace(Some(Vec::new()));
        let polled = dispatcher::with_default(listener(), || this.attempt.as_mut().poll(cx));
        let lines = LINES.replace(before).unwrap_or_default();
        for retry in lines.iter().filter_map(|line| retry_in(line)) {
            this.count += 1;
            (this.tell)(retry);
        }
        polled.map(|answer| (answer, this.count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_retry_line_says_which_why_and_how_long() {
        let retry = retry_in(
            "Request failed, retrying (2/3): RateLimitExceeded { details: \"slow down\", \
             retry_delay: Some(1.5s) }",
        )
        .unwrap();
        assert_eq!(retry.attempt, 2);
        assert_eq!(retry.error.kind(), ProviderErrorKind::RateLimit);
        assert_eq!(retry.asked, Some(Duration::from_millis(1500)));

        let refused =
            retry_in("Request failed, retrying (1/3): NetworkError(\"connection refused\")")
                .unwrap();
        assert_eq!(refused.error.kind(), ProviderErrorKind::Unreachable);
        assert_eq!(refused.asked, None);
        assert!(refused.error.to_string().contains("connection refused"));

        let server = retry_in("Request failed, retrying (3/3): ServerError(\"500\")").unwrap();
        assert_eq!(server.error.kind(), ProviderErrorKind::Other);
        assert_eq!(asked_in("retry_delay: Some(0ns)"), Some(Duration::ZERO));
        assert_eq!(
            asked_in("retry_delay: Some(250ms)"),
            Some(Duration::from_millis(250))
        );
        assert_eq!(asked_in("retry_delay: None"), None);
    }

    #[test]
    fn another_line_is_no_retry() {
        assert_eq!(retry_in("something else failed"), None);
        assert_eq!(retry_in("retrying (x/3): what"), None);
    }
}
