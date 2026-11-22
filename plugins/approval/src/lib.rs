//! Permission modes for the model's tool calls, after Goose: a gate at `tool/call`.
//!
//! | Mode | Read-only tools | Other tools |
//! | --- | --- | --- |
//! | `auto` | run | run |
//! | `smart` (default) | run | wait for a person |
//! | `approve` | wait for a person | wait for a person |
//! | `chat` | refused | refused |
//!
//! Tools on the `allow` list always run and tools on the `deny` list never do, whatever the
//! mode; `deny` wins. Whether a tool only reads is what the tool itself declares, so `smart`
//! trusts every plugin's word for it: where a plugin's tools are not to be taken at their
//! word, use `approve`, or put those tools on the `deny` list.
//!
//! The plugin imports no capability: it holds no authority and only answers. Asking a person
//! is the harness's job; this plugin only says when to.

mod policy;

use std::sync::OnceLock;

use corduroy_guest::{Plugin, PluginInfo, Verdict};
use policy::{Decision, Policy};
use serde::Deserialize;

/// The decision point this plugin guards.
const TOOL_CALL: &str = "tool/call";

static POLICY: OnceLock<Policy> = OnceLock::new();

/// The part of a `tool/call` request this plugin reads.
#[derive(Deserialize)]
struct Call {
    tool: String,
    #[serde(rename = "read-only", default)]
    read_only: bool,
}

struct Approval;

impl Plugin for Approval {
    fn activate(config: String) -> Result<PluginInfo, String> {
        let policy: Policy = corduroy_guest::config(&config)?;
        // A second activation of the same instance keeps the first policy.
        let _ = POLICY.set(policy);
        Ok(PluginInfo::new("approval", env!("CARGO_PKG_VERSION")).guard(TOOL_CALL))
    }

    fn gate(point: String, request: String) -> Verdict {
        if point != TOOL_CALL {
            return Verdict::Allow;
        }
        // A request this plugin cannot read is one it cannot vouch for.
        let Ok(call) = serde_json::from_str::<Call>(&request) else {
            return Verdict::Deny("the approval gate could not read the call".to_owned());
        };
        let Some(policy) = POLICY.get() else {
            return Verdict::Deny("the approval gate was not activated".to_owned());
        };
        match policy.decide(&call.tool, call.read_only) {
            Decision::Allow => Verdict::Allow,
            Decision::Ask(reason) => Verdict::Ask(reason),
            Decision::Deny(reason) => Verdict::Deny(reason),
        }
    }
}

corduroy_guest::export!(Approval);
