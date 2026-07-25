//! Criterion benchmarks for the budgeting hot path (DX-302): the
//! [`HeuristicTokenizer`] runs on every context-compaction pass before every
//! model call, over the entire conversation history — a throughput regression
//! here taxes every agent step. CI runs these with `--output-format bencher`
//! and `github-action-benchmark` flags a regression against the cached
//! baseline.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use wovyr_provider::{HeuristicTokenizer, Message, TokenCounter};

/// ~1 KiB of representative English prose with punctuation and numerals.
const PROSE: &str = "The workflow engine persists an event and a full checkpoint per step, \
so resume is idempotent from any store; per-execution locks serialize resume and deliver. \
Wire serialization is snake_case, activities are capped at 1 MiB (fail-closed), and logged \
events are wrapped in a {\"v\": N} version envelope — newer-than-understood is rejected. \
Durable timers pin fire_at on first suspend (2026-07-17T00:00:00Z, offset +05:30), and the \
dispatcher polls every 5 seconds by default. Costs: $0.0000335 for 67 tokens on the mock \
provider, priced by the PriceBook's prefix lookup. ";

fn bench_count(c: &mut Criterion) {
    let tokenizer = HeuristicTokenizer;
    let kb = PROSE.to_string();
    let sixty_four_kb = PROSE.repeat(64);

    c.bench_function("tokenizer_count_1kb_prose", |b| {
        b.iter(|| tokenizer.count(black_box(&kb)))
    });
    // A long history — the compaction-time shape, where throughput matters.
    c.bench_function("tokenizer_count_64kb_prose", |b| {
        b.iter(|| tokenizer.count(black_box(&sixty_four_kb)))
    });
}

fn bench_count_messages(c: &mut Criterion) {
    let tokenizer = HeuristicTokenizer;
    // A plausible tool-loop history: 40 alternating turns of varying length.
    let history: Vec<Message> = (0..40)
        .map(|i| {
            let content = PROSE.repeat(1 + i % 4);
            if i % 2 == 0 {
                Message::user(content)
            } else {
                Message::assistant(content)
            }
        })
        .collect();

    c.bench_function("tokenizer_count_40_message_history", |b| {
        b.iter(|| {
            history
                .iter()
                .map(|m| tokenizer.count_message(black_box(m)))
                .sum::<usize>()
        })
    });
}

criterion_group!(benches, bench_count, bench_count_messages);
criterion_main!(benches);
