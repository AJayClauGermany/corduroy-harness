//! Gates: plugins that decide whether something may happen.
//!
//! Events report what happened and expect no answer. Where the loop needs an answer before
//! it goes on, it asks the gates instead. The core names a small, fixed set of *decision
//! points* ([`POINTS`]); a gate registered in the [`Gates`] registry is asked at each of
//! them and either gives a [`Verdict`] or has no opinion.
//!
//! Every gate with an opinion is asked, in name order, and the strictest verdict wins: deny
//! over ask over allow. The outcome therefore does not depend on which plugin was loaded
//! first, and a permissive gate cannot overrule a strict one.

use std::fmt;

use crate::registry::Registry;

/// Name of the service under which the host provides the [`Gates`] registry.
pub const GATES: &str = "gates";

/// The decision point asked before a tool call runs.
///
/// Its request is JSON text: `{"session", "id", "tool", "input", "read-only", "caller"}`,
/// where `caller` is `agent` for the model's calls and `plugin:<name>` for a plugin's.
pub const TOOL_CALL: &str = "tool/call";

/// Every decision point the host asks gates at.
pub const POINTS: [&str; 1] = [TOOL_CALL];

/// The registry of every gate, asked in name order.
pub type Gates = Registry<dyn Gate>;

/// A gate's answer at a decision point.
///
/// Verdicts are ordered by strictness, so the strictest of several is their maximum.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    /// Let it happen.
    Allow,
    /// Let it happen only once a person has agreed. The reason is shown to them.
    Ask(String),
    /// Refuse it. The reason is shown to the model so that it can try something else.
    Deny(String),
}

impl Verdict {
    /// `allow`, `ask` or `deny`.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask(_) => "ask",
            Self::Deny(_) => "deny",
        }
    }

    /// Why the gate asked or denied; empty for an allow.
    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            Self::Allow => "",
            Self::Ask(reason) | Self::Deny(reason) => reason,
        }
    }

    /// Ranks verdicts by strictness: deny over ask over allow. Within one kind the reason
    /// does not count, so the first gate to give the strictest kind is the one recorded.
    const fn strictness(&self) -> u8 {
        match self {
            Self::Allow => 0,
            Self::Ask(_) => 1,
            Self::Deny(_) => 2,
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Allow => f.write_str("allow"),
            Self::Ask(reason) => write!(f, "ask: {reason}"),
            Self::Deny(reason) => write!(f, "deny: {reason}"),
        }
    }
}

/// The outcome of asking the gates at one decision point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// The strictest verdict given.
    pub verdict: Verdict,
    /// The name the gate that gave it is registered under.
    pub gate: String,
}

/// Something that decides whether things may happen at one or more decision points.
pub trait Gate: Send + Sync {
    /// The verdict on `request` at `point`, or `None` if this gate does not guard `point` or
    /// has no opinion on this request.
    ///
    /// A gate that cannot reach a decision (it failed, or it trapped) should deny: a gate
    /// fails closed.
    fn check(&self, point: &str, request: &str) -> Option<Verdict>;
}

impl<F> Gate for F
where
    F: Fn(&str, &str) -> Option<Verdict> + Send + Sync,
{
    fn check(&self, point: &str, request: &str) -> Option<Verdict> {
        self(point, request)
    }
}

impl Registry<dyn Gate> {
    /// Ask every gate about `request` at `point`, in name order. Returns the strictest
    /// verdict and the gate that gave it first, or `None` if no gate had an opinion.
    #[must_use]
    pub fn decide(&self, point: &str, request: &str) -> Option<Decision> {
        let mut decision: Option<Decision> = None;
        for name in self.names() {
            // The registry lock is not held while a gate runs, so a gate may itself look at
            // the registry.
            let Some(verdict) = self.get(&name).and_then(|gate| gate.check(point, request)) else {
                continue;
            };
            let stricter = decision
                .as_ref()
                .is_none_or(|held| verdict.strictness() > held.verdict.strictness());
            if stricter {
                decision = Some(Decision {
                    verdict,
                    gate: name,
                });
            }
        }
        decision
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn always(verdict: Verdict) -> Arc<dyn Gate> {
        Arc::new(move |point: &str, _: &str| (point == TOOL_CALL).then(|| verdict.clone()))
    }

    #[test]
    fn no_gate_means_no_decision() {
        let gates = Gates::new();
        assert_eq!(gates.decide(TOOL_CALL, "{}"), None);

        gates
            .insert("quiet", Arc::new(|_: &str, _: &str| None))
            .unwrap();
        assert_eq!(gates.decide(TOOL_CALL, "{}"), None);
    }

    #[test]
    fn the_strictest_verdict_wins_whatever_the_order() {
        let gates = Gates::new();
        gates.insert("a-allow", always(Verdict::Allow)).unwrap();
        gates
            .insert("b-ask", always(Verdict::Ask("sure?".into())))
            .unwrap();
        assert_eq!(
            gates.decide(TOOL_CALL, "{}"),
            Some(Decision {
                verdict: Verdict::Ask("sure?".into()),
                gate: "b-ask".into()
            })
        );

        gates
            .insert("0-deny", always(Verdict::Deny("no".into())))
            .unwrap();
        gates
            .insert("z-deny", always(Verdict::Deny("never".into())))
            .unwrap();
        let decision = gates.decide(TOOL_CALL, "{}").unwrap();
        assert_eq!(decision.verdict, Verdict::Deny("no".into()));
        assert_eq!(decision.gate, "0-deny");
    }

    #[test]
    fn a_gate_is_asked_only_at_its_points() {
        let gates = Gates::new();
        gates
            .insert("tools-only", always(Verdict::Deny("no".into())))
            .unwrap();
        assert_eq!(gates.decide("model/request", "{}"), None);
    }

    #[test]
    fn verdicts_describe_themselves() {
        assert_eq!(Verdict::Allow.to_string(), "allow");
        assert_eq!(Verdict::Ask("why".into()).kind(), "ask");
        assert_eq!(Verdict::Deny("because".into()).reason(), "because");
        assert_eq!(Verdict::Allow.reason(), "");
        assert!(Verdict::Deny(String::new()) > Verdict::Ask("z".into()));
    }
}
