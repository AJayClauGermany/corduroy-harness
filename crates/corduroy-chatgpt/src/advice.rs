//! What to tell a person when a request paid by their plan fails.
//!
//! The service answers such a failure with a code (`subscription_sharing_…`), and a tool is
//! to say what the person can do about it without retrying in a loop, switching to another
//! way of paying, or sending them to sign in again where that changes nothing. A failure
//! reaches the command as text, and not every layer keeps the code, so where there is none
//! the status is gone by: a guess, and worded as one.

use crate::{SIGN_IN, USAGE_URL};

/// The line shown while an agent's requests are paid by the plan.
#[must_use]
pub fn using() -> String {
    format!("Using ChatGPT plan. Manage usage: {USAGE_URL}")
}

/// What the person can do about `failure`, the reason a run failed, if it reads like one
/// of the plan's.
#[must_use]
pub fn advice(failure: &str) -> Option<String> {
    let failure = failure.to_ascii_lowercase();
    let has = |part: &str| failure.contains(part);
    let said = if has("subscription_sharing_usage_limit_exceeded") {
        format!(
            "Your ChatGPT plan's usage limit was reached, for this tool or for the plan. \
             Nothing is retried and nothing else is charged: the run can be asked again once \
             the limit resets. See and manage usage: {USAGE_URL}. To go on before then, ask a \
             provider that has a key of its own (`--provider <name>` for one in the agent \
             file)."
        )
    } else if has("subscription_sharing_user_not_eligible") {
        "Your account, its workspace or its policy does not allow paying with the ChatGPT \
         plan here. Signing in again will not change that."
            .to_owned()
    } else if has("subscription_sharing_usage_unavailable")
        || has("subscription_sharing_user_unavailable")
    {
        "The ChatGPT plan could not be checked just now. Your sign-in is kept: try again \
         later."
            .to_owned()
    } else if has("subscription_sharing_unsupported_capability") {
        "The request used something a ChatGPT plan does not pay for: a `max_tokens` on the \
         provider, a tool or a model. It will be refused again until that is removed."
            .to_owned()
    } else if has("subscription_sharing_route_not_supported") {
        "A ChatGPT plan pays for OpenAI's Responses API only: use a model and a `base_url` \
         that go there."
            .to_owned()
    } else if has("subscription_sharing_invalid_user") || has("chatpass_v2_") {
        format!(
            "The ChatGPT sign-in was not accepted. If it was disconnected in ChatGPT's \
             settings, sign in again with `{SIGN_IN}`."
        )
    } else if has("rate limit") || has("usage limit") || has("429") {
        format!("If this is your ChatGPT plan's usage limit, see and change it at {USAGE_URL}")
    } else if has("authentication failed") || has("401") {
        format!(
            "If the ChatGPT sign-in was disconnected in ChatGPT's settings, sign in again \
             with `{SIGN_IN}`."
        )
    } else {
        return None;
    };
    Some(said)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_failure_is_answered_by_its_code() {
        let limit = advice(
            "Responses API failed: Object {\"code\": String(\"subscription_sharing_usage_limit_exceeded\")}",
        )
        .unwrap();
        assert!(limit.contains(USAGE_URL), "{limit}");
        // What OpenAI answers word for word, as the command is handed it.
        let answered = advice(
            "Request failed: Responses API error: {\"message\":\"The ChatGPT user has reached \
             their Subscription Sharing usage limit. Ask the user to try again after their \
             usage limit resets or use an API key instead.\",\"type\":\
             \"invalid_request_error\",\"param\":null,\"code\":\
             \"subscription_sharing_usage_limit_exceeded\"}",
        );
        assert_eq!(answered.as_deref(), Some(limit.as_str()));
        assert!(
            limit.contains("resets") && limit.contains("--provider"),
            "{limit}"
        );
        let ineligible = advice("403 subscription_sharing_user_not_eligible").unwrap();
        assert!(ineligible.contains("will not change"), "{ineligible}");
        assert!(!ineligible.contains(SIGN_IN));
        let later = advice("subscription_sharing_usage_unavailable").unwrap();
        assert!(later.contains("try again later"), "{later}");
        let invalid = advice("subscription_sharing_invalid_user").unwrap();
        assert!(invalid.contains(SIGN_IN), "{invalid}");
    }

    #[test]
    fn without_a_code_the_status_is_a_guess_and_anything_else_is_left_alone() {
        let guess = advice("Rate limit exceeded: Too Many Requests").unwrap();
        assert!(
            guess.starts_with("If this is") && guess.contains(USAGE_URL),
            "{guess}"
        );
        let signed_out = advice("Authentication failed for x. Status: 401").unwrap();
        assert!(signed_out.contains(SIGN_IN), "{signed_out}");
        assert_eq!(advice("Network error: connection reset"), None);
        assert_eq!(advice("System messages are not allowed"), None);
    }
}
