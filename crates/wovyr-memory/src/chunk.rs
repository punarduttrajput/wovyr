//! Document chunking for parent-document retrieval (RM-AIM-P2 RAG-201).
//!
//! A long document embedded as one vector gets a *diluted* embedding — every
//! topic in it pulls the vector toward the mean, so no single query matches it
//! well. [`split`] cuts a document into overlapping windows sized for
//! embedding, each stored as its own retrieval unit
//! ([`MemoryRecord`](crate::MemoryRecord)) linked back to the full parent
//! document via `parent_id`.
//!
//! The splitter is a pure, deterministic function (house rule: no
//! clock/randomness in core logic). Windows are measured in **characters with
//! word-boundary snapping** — the same "documented estimate, not billing"
//! stance as `wovyr_provider`'s `HeuristicTokenizer`: characters are a
//! deterministic, dependency-free proxy for tokens (~4 chars/token for
//! English), and a chunk never splits a word in half.

use serde::{Deserialize, Serialize};

/// How to split a document into chunks.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ChunkPolicy {
    /// Maximum chunk size in characters (word-boundary snapped, so a chunk may
    /// come in under this; a single word longer than the window is kept whole
    /// rather than split mid-word).
    pub max_chars: usize,
    /// Overlap between consecutive chunks in characters, so a fact straddling
    /// a boundary appears whole in at least one chunk. Clamped below
    /// `max_chars` so every step makes forward progress.
    pub overlap_chars: usize,
}

impl Default for ChunkPolicy {
    /// ~300 tokens per chunk with ~50 tokens of overlap at the ~4 chars/token
    /// heuristic — the common dense-retrieval sweet spot.
    fn default() -> Self {
        Self {
            max_chars: 1200,
            overlap_chars: 200,
        }
    }
}

/// Split `content` into overlapping, word-boundary-snapped chunks per `policy`.
///
/// Whitespace runs (including newlines) normalize to single spaces — chunks
/// are embedding/retrieval units, not a byte-faithful rendition; the verbatim
/// document lives on the parent record. Deterministic: the same input and
/// policy always produce the same chunks. Content that fits in one window
/// returns a single chunk; empty/whitespace-only content returns none.
pub fn split(content: &str, policy: &ChunkPolicy) -> Vec<String> {
    let words: Vec<&str> = content.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    let max_chars = policy.max_chars.max(1);
    // Overlap must leave room to advance, or the loop would re-emit the same
    // window forever.
    let overlap = policy.overlap_chars.min(max_chars.saturating_sub(1));

    let mut chunks = Vec::new();
    let mut start = 0usize; // word index
    loop {
        // Grow the window word by word until the next word would overflow it.
        // Always take at least one word so an over-long word can't stall us.
        let mut end = start;
        let mut len = 0usize;
        while end < words.len() {
            let add = words[end].chars().count() + usize::from(end > start);
            if len + add > max_chars && end > start {
                break;
            }
            len += add;
            end += 1;
        }
        chunks.push(words[start..end].join(" "));
        if end >= words.len() {
            return chunks;
        }
        // Step the next window back over the tail of this one until the
        // overlap budget is covered — but never back to (or before) `start`,
        // so consecutive windows always advance.
        let mut next = end;
        let mut covered = 0usize;
        while next > start + 1 && covered < overlap {
            next -= 1;
            covered += words[next].chars().count() + 1;
        }
        start = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(max_chars: usize, overlap_chars: usize) -> ChunkPolicy {
        ChunkPolicy {
            max_chars,
            overlap_chars,
        }
    }

    #[test]
    fn short_content_is_a_single_chunk() {
        let chunks = split("a small note", &ChunkPolicy::default());
        assert_eq!(chunks, vec!["a small note"]);
    }

    #[test]
    fn empty_and_whitespace_only_content_yield_no_chunks() {
        assert!(split("", &ChunkPolicy::default()).is_empty());
        assert!(split("   \n\t  ", &ChunkPolicy::default()).is_empty());
    }

    #[test]
    fn long_content_splits_into_windows_under_the_size_cap() {
        let doc = "word ".repeat(100); // 100 5-char units
        let chunks = split(&doc, &policy(50, 0));
        assert!(chunks.len() > 1, "long doc must split");
        for c in &chunks {
            assert!(
                c.chars().count() <= 50,
                "chunk exceeds max_chars: {} chars",
                c.chars().count()
            );
        }
    }

    #[test]
    fn consecutive_chunks_overlap_when_requested() {
        let doc: String = (0..40)
            .map(|i| format!("w{i:02}"))
            .collect::<Vec<_>>()
            .join(" ");
        let chunks = split(&doc, &policy(40, 12));
        assert!(chunks.len() > 1);
        for pair in chunks.windows(2) {
            let tail_word = pair[0].split_whitespace().last().unwrap();
            assert!(
                pair[1].split_whitespace().any(|w| w == tail_word),
                "chunk {:?} does not overlap the tail of {:?}",
                pair[1],
                pair[0]
            );
        }
    }

    #[test]
    fn zero_overlap_partitions_without_repeats() {
        let doc: String = (0..30)
            .map(|i| format!("w{i:02}"))
            .collect::<Vec<_>>()
            .join(" ");
        let chunks = split(&doc, &policy(40, 0));
        let rejoined: Vec<&str> = chunks.iter().flat_map(|c| c.split_whitespace()).collect();
        let originals: Vec<&str> = doc.split_whitespace().collect();
        assert_eq!(rejoined, originals, "zero overlap must partition exactly");
    }

    #[test]
    fn every_word_appears_in_some_chunk() {
        let doc: String = (0..75)
            .map(|i| format!("token{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let chunks = split(&doc, &policy(60, 15));
        for word in doc.split_whitespace() {
            assert!(
                chunks
                    .iter()
                    .any(|c| c.split_whitespace().any(|w| w == word)),
                "word {word} lost during chunking"
            );
        }
    }

    #[test]
    fn a_word_longer_than_the_window_is_kept_whole_and_progress_continues() {
        let doc = format!("short {} tail", "x".repeat(100));
        let chunks = split(&doc, &policy(20, 5));
        assert!(chunks.iter().any(|c| c.contains(&"x".repeat(100))));
        assert!(chunks.iter().any(|c| c.contains("tail")));
    }

    #[test]
    fn pathological_overlap_still_terminates_and_advances() {
        // overlap >= max_chars would re-emit the same window forever without
        // the clamp; assert it terminates and still covers the whole doc.
        let doc: String = (0..20)
            .map(|i| format!("w{i:02}"))
            .collect::<Vec<_>>()
            .join(" ");
        let chunks = split(&doc, &policy(10, 10_000));
        assert!(chunks.last().unwrap().contains("w19"));
    }

    #[test]
    fn splitting_is_deterministic() {
        let doc = "alpha beta gamma delta ".repeat(50);
        let a = split(&doc, &ChunkPolicy::default());
        let b = split(&doc, &ChunkPolicy::default());
        assert_eq!(a, b);
    }

    #[test]
    fn multibyte_characters_do_not_panic_or_split_mid_word() {
        let doc = "héllo wörld übung ".repeat(30);
        let chunks = split(&doc, &policy(40, 10));
        assert!(chunks.len() > 1);
        for c in &chunks {
            for w in c.split_whitespace() {
                assert!(
                    ["héllo", "wörld", "übung"].contains(&w),
                    "word was split mid-character: {w:?}"
                );
            }
        }
    }
}
