//! Context-window management: token-budgeted history compaction.
//!
//! `run_agent` grows the message history every tool turn (an assistant tool-call
//! message plus one tool-result message per call). Left unbounded, a long tool loop
//! eventually exceeds the model's context window — a hard provider error, or a
//! silently truncated prompt and a runaway bill (PRD-004 R-A.1 / RM-AIM-P1 AIC-101).
//!
//! [`compact`] keeps the prompt under a token budget by dropping the **oldest tool
//! rounds** first, while always preserving the leading system prompt(s) and the first
//! user turn (the acceptance-criterion invariant). The default strategy is lossless
//! drop-oldest ([`CompactionStrategy::DropOldest`]); it is deterministic (a pure
//! function of its inputs — no clock, no randomness), matching the house rule.
//!
//! A "tool round" is the coherent unit the OpenAI-family wire format requires: an
//! `assistant` message carrying `tool_calls` followed by the `tool` result messages
//! answering them. Dropping a round whole keeps every retained `tool` message paired
//! with its originating `assistant` call, so the compacted history is still valid to
//! send (a dangling tool result, or a tool call with no results, is a 400).

use apex_provider::{Message, Role, TokenCounter};

/// How the agent keeps its prompt within the model's context window.
#[derive(Debug, Clone)]
pub struct ContextPolicy {
    /// Token budget the assembled prompt (all messages + advertised tool specs) may
    /// occupy before compaction drops the oldest tool rounds. Chosen generously by
    /// default so short runs are never touched; lower it for a smaller-window model.
    pub max_prompt_tokens: usize,
    /// Which compaction strategy to apply when over budget.
    pub strategy: CompactionStrategy,
}

/// Compaction strategy applied once the prompt exceeds [`ContextPolicy::max_prompt_tokens`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionStrategy {
    /// Drop the oldest tool rounds until the prompt fits (lossless w.r.t. the system
    /// prompt + first user turn + most-recent rounds; the dropped middle is gone).
    DropOldest,
}

impl Default for ContextPolicy {
    fn default() -> Self {
        Self {
            // ~128k-token class window with headroom for the completion; well above
            // any short run, so existing single-/few-step agents are unaffected.
            max_prompt_tokens: 96_000,
            strategy: CompactionStrategy::DropOldest,
        }
    }
}

/// The outcome of a compaction pass — the (possibly shortened) history plus what was
/// dropped, for the caller to log/emit.
#[derive(Debug)]
pub struct Compacted {
    /// The messages to send this turn.
    pub messages: Vec<Message>,
    /// How many messages were dropped (0 if the prompt already fit).
    pub dropped_messages: usize,
    /// Token estimate of the prompt after compaction (messages + `tools_overhead`).
    pub tokens_after: usize,
}

/// Compact `messages` to fit `policy.max_prompt_tokens`, accounting for the token
/// cost of the advertised tool specs (`tools_overhead`) which ride in the same
/// request. Preserves the leading system message(s) and the first user turn no matter
/// what; drops whole tool rounds oldest-first from the middle.
pub fn compact(
    messages: Vec<Message>,
    tools_overhead: usize,
    policy: &ContextPolicy,
    counter: &dyn TokenCounter,
) -> Compacted {
    let CompactionStrategy::DropOldest = policy.strategy;

    let total = |msgs: &[Message]| -> usize {
        tools_overhead + msgs.iter().map(|m| counter.count_message(m)).sum::<usize>()
    };

    let original_len = messages.len();
    if total(&messages) <= policy.max_prompt_tokens {
        let tokens_after = total(&messages);
        return Compacted {
            messages,
            dropped_messages: 0,
            tokens_after,
        };
    }

    // Split into the always-kept preamble (leading system messages + first user turn)
    // and the tail of tool rounds.
    let preamble_end = preamble_end(&messages);
    let mut preamble: Vec<Message> = messages;
    let tail: Vec<Message> = preamble.split_off(preamble_end);
    let mut rounds = group_rounds(tail);

    // Drop the oldest rounds until we fit or nothing droppable remains. Rebuild the
    // candidate each step so the token check reflects the real message set.
    while !rounds.is_empty() {
        let candidate_tokens = tools_overhead
            + preamble
                .iter()
                .chain(rounds.iter().flatten())
                .map(|m| counter.count_message(m))
                .sum::<usize>();
        if candidate_tokens <= policy.max_prompt_tokens {
            break;
        }
        rounds.remove(0);
    }

    let mut out = preamble;
    for round in rounds {
        out.extend(round);
    }
    let tokens_after = total(&out);
    Compacted {
        dropped_messages: original_len - out.len(),
        tokens_after,
        messages: out,
    }
}

/// Index one past the always-kept preamble: all leading `System` messages plus the
/// first `User` message (if any). Everything after is compactable tool rounds.
fn preamble_end(messages: &[Message]) -> usize {
    let mut i = 0;
    while i < messages.len() && messages[i].role == Role::System {
        i += 1;
    }
    // Include the first user turn.
    if i < messages.len() && messages[i].role == Role::User {
        i += 1;
    }
    i
}

/// Group a message tail into tool rounds. Each round begins at an `Assistant` message
/// and runs up to (but not including) the next `Assistant` message, so an assistant
/// tool-call message stays with the tool-result messages that answer it. A leading
/// run before the first `Assistant` (shouldn't occur in the agent loop, but handled
/// defensively) forms its own group.
fn group_rounds(tail: Vec<Message>) -> Vec<Vec<Message>> {
    let mut rounds: Vec<Vec<Message>> = Vec::new();
    for msg in tail {
        if msg.role == Role::Assistant || rounds.is_empty() {
            rounds.push(vec![msg]);
        } else {
            rounds.last_mut().unwrap().push(msg);
        }
    }
    rounds
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_provider::{HeuristicTokenizer, ToolCall};

    fn asst_call(id: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: None,
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: "search".into(),
                arguments: format!("{{\"q\":\"query number {id} with some padding text\"}}"),
            }],
            tool_call_id: None,
            name: None,
        }
    }

    fn tool_res(id: &str) -> Message {
        Message::tool_result(
            id,
            "search",
            format!("result body for call {id} ").repeat(20),
        )
    }

    /// A synthetic long tool loop: the compacted request stays under budget while the
    /// system prompt and the first user turn are always preserved (AIC-101 acceptance).
    #[test]
    fn long_tool_loop_stays_under_budget_and_keeps_system_and_user() {
        let counter = HeuristicTokenizer;
        let mut messages = vec![
            Message::system("You are a careful research assistant. Follow instructions."),
            Message::user("Tell me everything about the history of the Roman aqueducts."),
        ];
        // 40 tool rounds — far past any small budget.
        for n in 0..40 {
            let id = format!("call{n}");
            messages.push(asst_call(&id));
            messages.push(tool_res(&id));
        }

        let policy = ContextPolicy {
            max_prompt_tokens: 400,
            strategy: CompactionStrategy::DropOldest,
        };
        let result = compact(messages, /* tools_overhead */ 20, &policy, &counter);

        assert!(
            result.tokens_after <= policy.max_prompt_tokens,
            "compacted prompt {} must be under budget {}",
            result.tokens_after,
            policy.max_prompt_tokens
        );
        assert!(
            result.dropped_messages > 0,
            "a huge loop must drop something"
        );
        // System prompt + first user turn survive at the front.
        assert_eq!(result.messages[0].role, Role::System);
        assert_eq!(result.messages[1].role, Role::User);
        assert!(
            result.messages[0]
                .content
                .as_deref()
                .unwrap()
                .contains("research assistant")
        );
        assert!(
            result.messages[1]
                .content
                .as_deref()
                .unwrap()
                .contains("Roman aqueducts")
        );
    }

    /// Retained history is still a valid wire sequence: no tool-result message is left
    /// without its originating assistant tool-call round.
    #[test]
    fn retained_rounds_stay_coherent() {
        let counter = HeuristicTokenizer;
        let mut messages = vec![
            Message::system("sys"),
            Message::user("do a lot of tool work please"),
        ];
        for n in 0..30 {
            let id = format!("c{n}");
            messages.push(asst_call(&id));
            messages.push(tool_res(&id));
            messages.push(tool_res(&format!("{id}b"))); // a second result in the round
        }
        let policy = ContextPolicy {
            max_prompt_tokens: 300,
            strategy: CompactionStrategy::DropOldest,
        };
        let result = compact(messages, 0, &policy, &counter);

        // Walk the tail: the first non-preamble message must be an Assistant, and every
        // Tool message must be preceded (somewhere in its round) by an Assistant.
        let mut seen_assistant = false;
        for m in &result.messages[2..] {
            match m.role {
                Role::Assistant => seen_assistant = true,
                Role::Tool => assert!(
                    seen_assistant,
                    "a tool result must follow its assistant call"
                ),
                other => panic!("unexpected role in tail: {other:?}"),
            }
        }
    }

    #[test]
    fn under_budget_is_untouched() {
        let counter = HeuristicTokenizer;
        let messages = vec![
            Message::system("sys"),
            Message::user("hi"),
            asst_call("c0"),
            tool_res("c0"),
        ];
        let before = messages.clone();
        let policy = ContextPolicy::default(); // huge budget
        let result = compact(messages, 0, &policy, &counter);
        assert_eq!(result.dropped_messages, 0);
        assert_eq!(result.messages.len(), before.len());
    }

    /// Determinism: identical input compacts to an identical result.
    #[test]
    fn compaction_is_deterministic() {
        let counter = HeuristicTokenizer;
        let build = || {
            let mut m = vec![Message::system("sys"), Message::user("go")];
            for n in 0..25 {
                let id = format!("c{n}");
                m.push(asst_call(&id));
                m.push(tool_res(&id));
            }
            m
        };
        let policy = ContextPolicy {
            max_prompt_tokens: 250,
            strategy: CompactionStrategy::DropOldest,
        };
        let a = compact(build(), 0, &policy, &counter);
        let b = compact(build(), 0, &policy, &counter);
        assert_eq!(a.dropped_messages, b.dropped_messages);
        assert_eq!(a.tokens_after, b.tokens_after);
        assert_eq!(a.messages.len(), b.messages.len());
    }
}
