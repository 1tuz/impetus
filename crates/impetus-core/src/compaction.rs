//! Durable context compaction — event/state transition, not silent rewrite.
//!
//! Compaction appends typed budget events and (optionally) a summary artifact.
//! Structural session state is captured separately from the text summary so
//! policy/cwd/budgets/parent identity never depend only on model prose.

use crate::provider::ProviderMessage;

/// Max chars kept per message line inside a deterministic summary.
const SUMMARY_LINE_CHARS: usize = 240;

/// How many trailing non-system messages survive compaction in the prompt list.
const KEEP_TAIL_MESSAGES: usize = 2;

/// Build a deterministic text summary of messages being folded out of the prompt.
pub fn summarize_messages(messages: &[ProviderMessage]) -> String {
    let mut lines = Vec::with_capacity(messages.len());
    for message in messages {
        let content = truncate_chars(message.content(), SUMMARY_LINE_CHARS);
        lines.push(format!("[{}] {content}", message.role()));
    }
    lines.join("\n")
}

/// Fold middle history into a summary message; keep leading system + tail.
///
/// Returns `(compacted_messages, summary_text)`. Event store history is never
/// deleted — callers append durable compaction events separately.
pub fn compact_provider_messages(messages: Vec<ProviderMessage>) -> (Vec<ProviderMessage>, String) {
    if messages.len() <= KEEP_TAIL_MESSAGES + 1 {
        let summary = summarize_messages(&messages);
        return (messages, summary);
    }

    let mut systems = Vec::new();
    let mut rest = Vec::new();
    for message in messages {
        if message.role() == "system" && rest.is_empty() {
            systems.push(message);
        } else {
            rest.push(message);
        }
    }

    if rest.len() <= KEEP_TAIL_MESSAGES {
        let mut out = systems;
        let summary = summarize_messages(&rest);
        out.extend(rest);
        return (out, summary);
    }

    let keep_tail = KEEP_TAIL_MESSAGES.min(rest.len());
    let split_at = rest.len() - keep_tail;
    let (folded, tail) = rest.split_at(split_at);
    let summary = summarize_messages(folded);

    let mut out = systems;
    out.push(ProviderMessage::user(format!(
        "[context compaction summary]\n{summary}"
    )));
    out.extend(tail.iter().cloned());
    (out, summary)
}

/// Rough token estimate used after compaction resets the budget counter.
pub fn estimate_tokens(text: &str) -> u64 {
    (text.len() / 4).max(1) as u64
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut truncated = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        truncated.push('…');
    }
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_keeps_system_and_tail() {
        let messages = vec![
            ProviderMessage::system("rules"),
            ProviderMessage::user("turn1"),
            ProviderMessage::assistant("a1"),
            ProviderMessage::user("turn2"),
            ProviderMessage::assistant("a2"),
            ProviderMessage::user("latest"),
        ];
        let (compacted, summary) = compact_provider_messages(messages);
        assert_eq!(compacted[0].role(), "system");
        assert!(
            compacted[1]
                .content()
                .contains("[context compaction summary]")
        );
        assert!(summary.contains("[user] turn1"));
        assert!(summary.contains("[assistant] a1"));
        assert_eq!(compacted.last().unwrap().content(), "latest");
        assert!(!summary.contains("latest") || compacted.iter().any(|m| m.content() == "latest"));
    }

    #[test]
    fn short_history_is_passthrough() {
        let messages = vec![
            ProviderMessage::user("only"),
            ProviderMessage::assistant("reply"),
        ];
        let (compacted, _) = compact_provider_messages(messages.clone());
        assert_eq!(compacted, messages);
    }
}
