//! Gates: plugins that decide whether something may happen.
//!
//! Events report what happened and expect no answer. Where something needs an answer before
//! it goes on, it asks the gates instead, at a named *decision point*. A gate registered in
//! [`Gates`] is asked at each point and either gives a [`Verdict`] or has no opinion.
//!
//! Every gate with an opinion is asked, in name order, and the strictest verdict wins: deny
//! over ask over allow. The outcome therefore does not depend on which plugin was loaded
//! first, and a permissive gate cannot overrule a strict one.
//!
//! **Decision points are declared.** The core declares the two it asks at itself
//! ([`POINTS`]). Whatever else asks the gates (a native plugin deciding whether a request
//! may leave, say) first [declares](Gates::declare) its point, with a description of the
//! request it sends. A point nobody declared does not exist: asking at one is denied, and a
//! gate that names one is a mistake its host can catch (a WebAssembly gate is refused), so a
//! policy never silently guards a point that is never asked.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;

use crate::context::Context;
use crate::registry::{Duplicate, Registry};

/// Name of the service under which the host provides the [`Gates`] registry.
pub const GATES: &str = "gates";

/// The decision point asked before a tool call runs.
///
/// Its request is JSON text: `{"session", "id", "tool", "input", "read-only", "caller"}`,
/// where `caller` is `agent` for the model's calls and `plugin:<name>` for a plugin's.
pub const TOOL_CALL: &str = "tool/call";

/// The decision point asked before a plugin loads another plugin.
///
/// Its request is JSON text: `{"loader", "name", "hash", "requires", "grants"}`, where
/// `hash` is the SHA-256 of the component's bytes in hex, `requires` the capabilities it
/// imports and `grants` maps each capability to `{"allow", "deny"}` as requested.
pub const PLUGIN_LOAD: &str = "plugin/load";

/// The decision points the core defines. Every [`Gates`] starts with them declared.
pub const POINTS: [&str; 2] = [TOOL_CALL, PLUGIN_LOAD];

/// The name a [`Decision`] carries when the registry itself refused, because the point
/// asked at was never declared.
pub const UNDECLARED: &str = "gates";

/// The gates, asked in name order, and the decision points they may be asked at.
pub struct Gates {
    gates: Arc<Registry<dyn Gate>>,
    /// Each declared point, with a description of what is decided there.
    points: Arc<Registry<str>>,
}

impl Default for Gates {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Gates {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Gates")
            .field("gates", &self.gates.names())
            .field("points", &self.points.names())
            .finish()
    }
}

impl Gates {
    /// No gates, and the core's decision points ([`POINTS`]) declared.
    #[must_use]
    pub fn new() -> Self {
        let points = Registry::new();
        for (point, description) in [
            (TOOL_CALL, "before a tool call runs"),
            (PLUGIN_LOAD, "before a plugin loads another plugin"),
        ] {
            // The registry is new, so neither name is taken.
            drop(points.insert(point, Arc::from(description)));
        }
        Self {
            gates: Arc::new(Registry::new()),
            points: Arc::new(points),
        }
    }

    /// Add a gate on behalf of the plugin behind `ctx`. It is removed when that plugin is
    /// unloaded.
    ///
    /// # Errors
    ///
    /// Returns [`Duplicate`] if `name` is taken.
    pub fn register(
        &self,
        ctx: &mut Context<'_>,
        name: impl Into<String>,
        gate: Arc<dyn Gate>,
    ) -> Result<(), Duplicate> {
        self.gates.register(ctx, name, gate)
    }

    /// Add a gate with no owner. Prefer [`register`](Self::register) inside plugins.
    ///
    /// # Errors
    ///
    /// Returns [`Duplicate`] if `name` is taken.
    pub fn insert(&self, name: impl Into<String>, gate: Arc<dyn Gate>) -> Result<(), Duplicate> {
        self.gates.insert(name, gate)
    }

    /// Remove a gate. Unknown names are ignored.
    pub fn remove(&self, name: &str) {
        drop(self.gates.remove(name));
    }

    /// The names of the registered gates, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.gates.names()
    }

    /// Declare the decision point `point` on behalf of the plugin behind `ctx`, which will
    /// ask the gates there. `description` says what is decided and what the request holds.
    /// The point goes away when the plugin is unloaded.
    ///
    /// # Errors
    ///
    /// Returns [`Duplicate`] if the point is already declared.
    pub fn declare(
        &self,
        ctx: &mut Context<'_>,
        point: impl Into<String>,
        description: &str,
    ) -> Result<(), Duplicate> {
        self.points.register(ctx, point, Arc::from(description))
    }

    /// Declare a decision point with no owner. Prefer [`declare`](Self::declare) inside
    /// plugins.
    ///
    /// # Errors
    ///
    /// Returns [`Duplicate`] if the point is already declared.
    pub fn declare_point(
        &self,
        point: impl Into<String>,
        description: &str,
    ) -> Result<(), Duplicate> {
        self.points.insert(point, Arc::from(description))
    }

    /// Whether `point` is a declared decision point.
    #[must_use]
    pub fn declared(&self, point: &str) -> bool {
        self.points.get(point).is_some()
    }

    /// The declared decision points, sorted.
    #[must_use]
    pub fn points(&self) -> Vec<String> {
        self.points.names()
    }

    /// What is decided at `point`, as whoever declared it described it.
    #[must_use]
    pub fn describe(&self, point: &str) -> Option<Arc<str>> {
        self.points.get(point)
    }
}

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

impl Gates {
    /// Ask every gate about `request` at `point`, in name order. Returns the strictest
    /// verdict and the gate that gave it first, or `None` if no gate had an opinion.
    ///
    /// A `point` nobody declared is denied, by [`UNDECLARED`], without any gate being asked:
    /// whoever asks must say so first.
    #[must_use]
    pub fn decide(&self, point: &str, request: &str) -> Option<Decision> {
        if !self.declared(point) {
            return Some(Decision {
                verdict: Verdict::Deny(format!("`{point}` is not a declared decision point")),
                gate: UNDECLARED.to_owned(),
            });
        }
        let mut decision: Option<Decision> = None;
        for name in self.gates.names() {
            // The registry lock is not held while a gate runs, so a gate may itself look at
            // the registry.
            let gate = self.gates.get(&name);
            let Some(verdict) = gate.and_then(|gate| gate.check(point, request)) else {
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
    use super::*;
    use alloc::string::ToString;

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
        assert_eq!(gates.decide(PLUGIN_LOAD, "{}"), None);
        assert_eq!(gates.names(), ["tools-only"]);
        gates.remove("tools-only");
        assert_eq!(gates.names(), Vec::<String>::new());
    }

    #[test]
    fn a_point_must_be_declared_before_it_is_asked_at() {
        let gates = Arc::new(Gates::new());
        assert_eq!(gates.points(), [PLUGIN_LOAD, TOOL_CALL]);
        let refused = gates.decide("http/request", "{}").unwrap();
        assert_eq!(refused.gate, UNDECLARED);
        assert_eq!(refused.verdict.kind(), "deny");

        // A plugin declares the point it asks at; the point goes when the plugin does.
        let mut app = crate::context::App::new();
        app.provide(GATES, Arc::clone(&gates)).unwrap();
        let asker = app.plugin(crate::context::plugin_fn("outbound", &[GATES], |ctx| {
            let gates = ctx.service::<Gates>(GATES)?;
            gates.declare(ctx, "http/request", "before a request leaves: {\"url\"}")?;
            let guard: Arc<dyn Gate> = Arc::new(|point: &str, _: &str| {
                (point == "http/request").then(|| Verdict::Ask("sure?".into()))
            });
            gates.register(ctx, "outbound", guard)?;
            Ok(())
        }));
        assert!(gates.declared("http/request"));
        assert_eq!(
            gates.describe("http/request").as_deref(),
            Some("before a request leaves: {\"url\"}")
        );
        assert_eq!(
            gates.decide("http/request", "{}").unwrap().verdict,
            Verdict::Ask("sure?".into())
        );
        assert!(gates.declare_point("http/request", "again").is_err());

        app.dispose(asker).unwrap();
        assert!(!gates.declared("http/request"));
        assert_eq!(gates.names(), Vec::<String>::new());
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
