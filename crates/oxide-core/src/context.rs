//! Context window management.
//!
//! Tracks an approximate token count for the running conversation and compacts
//! it when it approaches the model's budget, so long sessions don't blow the
//! context window. The system message and the most recent turns are always
//! kept; the oldest middle messages are dropped first. Estimation is a cheap
//! chars/4 heuristic — good enough to decide *when* to compact without pulling
//! in a tokenizer.

use oxide_providers::{Message, Role};

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
        Message {
            role,
            content: "x".repeat(n),
        }
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
