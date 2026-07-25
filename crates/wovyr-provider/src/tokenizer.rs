//! Token counting for context-window budgeting.
//!
//! The agent loop needs to know, *before* sending a [`crate::ChatRequest`], roughly
//! how many tokens the assembled prompt will occupy so it can keep it under the
//! model's context window ([token-management](../../docs/05-llm-gateway/token-management.md),
//! PRD-004 R-A.1 / RM-AIM-P1 AIC-101). Before this module existed there was no
//! tokenizer anywhere — the mock estimated `chars/4` and the runtime cloned the whole
//! history unbudgeted, so a long tool loop silently blew the window and the bill.
//!
//! [`TokenCounter`] is the abstraction; [`HeuristicTokenizer`] is a **dependency-free,
//! deterministic** default. It is deliberately *not* a real byte-pair encoder: a
//! bundled BPE vocab is a heavy dependency and this workspace builds offline, so the
//! estimate approximates BPE granularity (each whitespace-delimited chunk splits into
//! alphanumeric runs — ~4 chars/subword-token — plus one token per punctuation/symbol
//! character) rather than reproducing it exactly. On typical English prose it lands
//! within ~10–20% of `cl100k_base`, which is the right precision for *budgeting* (stay
//! under the window with headroom) but not for billing — real cost comes from the
//! provider's returned `usage` (RM-AIM-P1 PRV-101), never from this estimate. A real
//! tokenizer can drop in behind [`TokenCounter`] later without touching callers.

use crate::types::Message;

/// Approximate per-message framing overhead (role tag + delimiters), mirroring the
/// ~3–4 tokens OpenAI-family chat formats add around every message.
pub const PER_MESSAGE_OVERHEAD: usize = 4;
/// Approximate framing overhead for one advertised tool (function wrapper), on top
/// of its name/description/schema content.
pub const PER_TOOL_OVERHEAD: usize = 8;

/// Counts tokens for a piece of text. The one seam a real tokenizer plugs into.
pub trait TokenCounter: Send + Sync {
    /// Estimated token count for `text`.
    fn count(&self, text: &str) -> usize;

    /// Estimated token count for a whole [`Message`], including role framing overhead
    /// and any tool-call name/argument payloads.
    fn count_message(&self, msg: &Message) -> usize {
        let mut total = PER_MESSAGE_OVERHEAD;
        if let Some(content) = &msg.content {
            total += self.count(content);
        }
        for call in &msg.tool_calls {
            total += self.count(&call.name) + self.count(&call.arguments) + PER_MESSAGE_OVERHEAD;
        }
        if let Some(name) = &msg.name {
            total += self.count(name);
        }
        total
    }
}

/// The default, dependency-free, deterministic token estimator (see module docs).
#[derive(Debug, Clone, Copy, Default)]
pub struct HeuristicTokenizer;

impl TokenCounter for HeuristicTokenizer {
    fn count(&self, text: &str) -> usize {
        let mut tokens = 0usize;
        for chunk in text.split_whitespace() {
            let mut alnum_run = 0usize;
            for c in chunk.chars() {
                if c.is_alphanumeric() {
                    alnum_run += 1;
                } else {
                    // Flush the pending word into ~4-char subword tokens, then count
                    // the punctuation/symbol char as its own token.
                    if alnum_run > 0 {
                        tokens += alnum_run.div_ceil(4);
                        alnum_run = 0;
                    }
                    tokens += 1;
                }
            }
            if alnum_run > 0 {
                tokens += alnum_run.div_ceil(4);
            }
        }
        // Non-empty text always costs at least one token.
        if tokens == 0 && !text.trim().is_empty() {
            1
        } else {
            tokens
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Message, ToolCall};

    #[test]
    fn empty_is_zero_whitespace_is_zero() {
        assert_eq!(HeuristicTokenizer.count(""), 0);
        assert_eq!(HeuristicTokenizer.count("   \n\t"), 0);
    }

    #[test]
    fn counts_scale_with_length_and_are_deterministic() {
        let t = HeuristicTokenizer;
        let short = t.count("hello world");
        let long = t.count(&"hello world ".repeat(100));
        assert!(long > short);
        // Deterministic: same input, same count.
        assert_eq!(
            t.count("the quick brown fox"),
            t.count("the quick brown fox")
        );
    }

    #[test]
    fn a_long_word_costs_more_than_one_token() {
        // "supercalifragilistic" is 20 alphanumeric chars ≈ 5 subword tokens (⌈20/4⌉).
        assert_eq!(HeuristicTokenizer.count("supercalifragilistic"), 5);
    }

    #[test]
    fn punctuation_counts_as_tokens() {
        // "a, b." -> "a"(1) + ","(1) + "b"(1) + "."(1) = 4.
        assert_eq!(HeuristicTokenizer.count("a, b."), 4);
    }

    #[test]
    fn message_includes_role_and_tool_overhead() {
        let plain = Message::user("hello there friend");
        let content_only = HeuristicTokenizer.count("hello there friend");
        assert_eq!(
            HeuristicTokenizer.count_message(&plain),
            content_only + PER_MESSAGE_OVERHEAD
        );

        let with_call = Message {
            role: crate::types::Role::Assistant,
            content: None,
            parts: Vec::new(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "search".into(),
                arguments: "{\"q\":\"rust\"}".into(),
            }],
            tool_call_id: None,
            name: None,
        };
        // Overhead + the tool call's name/args + one per-call framing overhead.
        let expected = PER_MESSAGE_OVERHEAD
            + HeuristicTokenizer.count("search")
            + HeuristicTokenizer.count("{\"q\":\"rust\"}")
            + PER_MESSAGE_OVERHEAD;
        assert_eq!(HeuristicTokenizer.count_message(&with_call), expected);
    }
}
