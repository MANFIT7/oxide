//! Context window management.
//!
//! Tracks an approximate token count for the running conversation and compacts
//! it when it approaches the model's budget, so long sessions don't blow the
//! context window. The system message and the most recent turns are always
//! kept; the oldest middle messages are dropped first. Estimation is a cheap
//! chars/4 heuristic — good enough to decide *when* to compact without pulling
//! in a tokenizer.

use oxide_providers::{Message, Role};
use std::collections::HashSet;

/// Drop dangling tool-call/tool-result pairs so the provider request never 400s.
///
/// Compaction, interrupts and rewinds can leave an assistant `tool_call` whose
/// result was removed, or a `Tool` result whose call was removed. Every provider
/// rejects such an orphan ("tool_result with no preceding tool_use" etc.). This
/// pass runs just before a request is built: it removes any `Tool` message whose
/// `tool_call_id` has no matching assistant `tool_call`, and neutralizes any
/// assistant `tool_call` that has no matching result (dropping the message if it
/// carried nothing but the call, else clearing the call so it serializes as
/// plain text). Returns true if anything changed.
pub fn sanitize_tool_pairs(messages: &mut Vec<Message>) -> bool {
    let call_ids: HashSet<String> = messages
        .iter()
        .filter_map(|m| m.tool_call.as_ref().map(|c| c.id.clone()))
        .collect();
    let result_ids: HashSet<String> = messages
        .iter()
        .filter_map(|m| m.tool_call_id.clone())
        .collect();

    let before = messages.len();
    // Drop tool results whose call is gone.
    messages.retain(|m| match &m.tool_call_id {
        Some(id) => call_ids.contains(id),
        None => true,
    });
    let after_results = messages.len();

    // For each assistant tool_call with no matching result, synthesize an
    // "interrupted" result right after it. This keeps the call/result pair valid
    // (so the provider never 400s on an orphan) AND tells the model the call did
    // NOT complete — so it won't silently re-issue the same call in a loop
    // (opencode's failUnsettledTools idea). Stripping the call instead left the
    // model free to repeat it endlessly.
    let mut out: Vec<Message> = Vec::with_capacity(messages.len() + 1);
    let mut synthesized = false;
    for m in messages.drain(..) {
        let dangling_id = m
            .tool_call
            .as_ref()
            .filter(|c| !result_ids.contains(&c.id))
            .map(|c| c.id.clone());
        out.push(m);
        if let Some(id) = dangling_id {
            out.push(Message::tool_result(
                "interrupted — the previous tool call did not complete. Do not repeat it; take a different approach or ask the user.",
                id,
            ));
            synthesized = true;
        }
    }
    *messages = out;

    before != after_results || synthesized
}

/// Roughly 4 characters per token for English/code.
pub fn estimate_tokens(messages: &[Message]) -> u64 {
    let chars: usize = messages.iter().map(|m| m.content.len()).sum();
    (chars / 4) as u64
}

/// Compact `messages` in place so the estimate stays under `max_tokens`.
///
/// Keeps the leading system message and the `keep_recent` newest messages,
/// dropping the oldest non-system messages until under budget. Returns the
/// number of messages dropped.
pub fn compact(messages: &mut Vec<Message>, max_tokens: u64, keep_recent: usize) -> u64 {
    if estimate_tokens(messages) <= max_tokens {
        return 0;
    }

    // Index of the first removable message: after a leading system message.
    let start = usize::from(matches!(
        messages.first().map(|m| m.role),
        Some(Role::System)
    ));
    let mut dropped = 0u64;

    while estimate_tokens(messages) > max_tokens {
        // Stop if only the system message + the recent tail remain.
        if messages.len() <= start + keep_recent {
            break;
        }
        // Remove the oldest removable (just after the system message).
        if start >= messages.len() {
            break;
        }
        messages.remove(start);
        dropped += 1;
    }
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxide_providers::{Message, Role};

    fn msg(role: Role, n: usize) -> Message {
        Message::new(role, "x".repeat(n))
    }

    #[test]
    fn no_compaction_under_budget() {
        let mut m = vec![msg(Role::System, 40), msg(Role::User, 40)];
        assert_eq!(compact(&mut m, 1000, 2), 0);
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn compaction_drops_oldest_keeps_system_and_recent() {
        // ~250 chars each => ~62 tokens each.
        let mut m = vec![
            msg(Role::System, 240),
            msg(Role::User, 240),
            msg(Role::Assistant, 240),
            msg(Role::User, 240),
            msg(Role::Assistant, 240),
        ];
        let before = estimate_tokens(&m);
        let dropped = compact(&mut m, before / 2, 2);
        assert!(dropped > 0, "should drop something");
        // System preserved at front.
        assert!(matches!(m[0].role, Role::System));
        // Recent tail preserved.
        assert!(m.len() >= 3);
        assert!(estimate_tokens(&m) <= before);
    }
}

#[cfg(test)]
mod pair_tests {
    use super::*;
    use oxide_providers::{Message, Role, ToolCall};

    fn call(id: &str) -> Message {
        let mut m = Message::new(Role::Assistant, "");
        m.tool_call = Some(ToolCall {
            id: id.to_string(),
            name: "x".to_string(),
            arguments: serde_json::json!({}),
        });
        m
    }
    fn result(id: &str) -> Message {
        let mut m = Message::new(Role::Tool, "ok");
        m.tool_call_id = Some(id.into());
        m
    }

    #[test]
    fn drops_orphan_tool_result() {
        let mut v = vec![result("a"), Message::new(Role::Assistant, "hi")];
        assert!(sanitize_tool_pairs(&mut v));
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn synthesizes_result_for_orphan_call() {
        let mut v = vec![call("a")];
        assert!(sanitize_tool_pairs(&mut v));
        // Call is kept and paired with a synthetic "interrupted" result so the
        // model sees it didn't complete (instead of being dropped + re-issued).
        assert_eq!(v.len(), 2);
        assert!(v[0].tool_call.is_some());
        assert_eq!(v[1].tool_call_id.as_deref(), Some("a"));
    }

    #[test]
    fn keeps_matched_pair() {
        let mut v = vec![call("a"), result("a")];
        assert!(!sanitize_tool_pairs(&mut v));
        assert_eq!(v.len(), 2);
    }
}
