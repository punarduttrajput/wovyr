<!--
File: docs/06-memory-engine/compression.md
Document ID: MEM-008
-->

# Memory Engine Compression

**Document ID:** MEM-008  
**File Path:** `docs/06-memory-engine/compression.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document specifies how the Memory Engine **compresses** retrieved memory so it fits the caller's token budget while preserving the most useful information.

Context windows and cost are finite. After [Retrieval](retrieval.md) and [Ranking](ranking.md) produce an ordered candidate set, compression decides what actually reaches the model. The [Context Manager](../04-agent-framework/context-manager.md) then assembles the final prompt.

---

# 2. Why Compress in the Engine

Compressing centrally (rather than in each agent) means:

- One implementation of summarization/dedup, consistently applied
- Token budgets enforced before content leaves the Engine
- Cached summaries reused across callers
- Cost attributed via the [LLM Gateway](../05-llm-gateway/index.md) for any
  model-assisted compression

---

# 3. Compression Techniques

| Technique | Effect | Cost |
|-----------|--------|------|
| Selection (top-K within budget) | Keep highest-ranked until budget hit | free |
| Semantic deduplication | Drop near-identical memories | cheap (vectors) |
| Extractive summarization | Keep key sentences | cheap |
| Abstractive summarization | LLM-rewritten condensed form | model call |
| Hierarchical summarization | Summaries-of-summaries for large sets | model calls |
| Field pruning | Strip non-essential metadata | free |

The Engine prefers cheap techniques first and only escalates to model-assisted
summarization when necessary to meet the budget.

---

# 4. Budget-Fitting Pipeline

```text
Ranked candidates + token_budget
   │
   ▼
1. Field pruning            (drop verbose metadata)
   │
   ▼
2. Semantic dedup           (collapse near-duplicates)
   │
   ▼
3. Greedy selection         (add top-ranked until budget)
   │  fits? ──► done
   ▼ over budget
4. Extractive summarization (shrink lowest-ranked kept items)
   │  fits? ──► done
   ▼ still over
5. Abstractive / hierarchical summarization (LLM Gateway)
   │
   ▼
Compressed context (≤ token_budget)
```

The token budget comes from the [query](memory-api.md#6-query-request)
(`token_budget`). If omitted, a tenant default applies.

---

# 5. Hierarchical Summarization

For very large candidate sets (e.g. an entire project's episodic history),
flat summarization loses structure. The Engine builds a tree:

```text
        Topic summary
       /      |       \
  Cluster   Cluster   Cluster summary
   /  \       |          /   \
 mem  mem    mem       mem   mem
```

Clusters are formed by embedding similarity; each cluster is summarized, then
cluster summaries are summarized into a topic summary. The caller receives the
level that fits the budget, with the option to "drill down" via follow-up queries.

---

# 6. Lossless vs. Lossy

| Mode | Guarantee | Use |
|------|-----------|-----|
| Lossless | Original text returned (only selection/dedup) | Compliance, legal, exact-quote needs |
| Lossy | Summarized/rewritten content | General context efficiency |

Callers choose via `compression.mode` (default `lossy`). Lossless mode never
rewrites content; it only selects and dedups, and may return fewer items if the
budget cannot hold full text.

---

# 7. Faithfulness & Attribution

Model-assisted summaries must not invent facts:

- Summaries are generated with low temperature and grounded strictly in the source
  memories.
- Each summary retains **source memory ids** so the caller can trace any statement
  back to its origin (and re-fetch full text).
- Optional verification re-checks the summary against sources and flags unsupported
  claims.

Faithfulness is a hard requirement — a summary that drops attribution is treated
as a defect.

---

# 8. Caching Summaries

Summaries are deterministic for a fixed (source set + budget + model), so they are
cached:

- Keyed by hash of source memory ids + version + budget + compression mode.
- Stored in Redis with TTL; invalidated when any source memory is updated.
- Reused across callers, avoiding repeated model spend.

This makes repeated retrieval of stable knowledge (e.g. onboarding docs) cheap.

---

# 9. Cost & Metering

Model-assisted compression issues calls through the
[LLM Gateway](../05-llm-gateway/index.md), so its token usage is metered and
attributed like any other inference (see
[LLM Gateway Token Management](../05-llm-gateway/token-management.md)). The Engine
reports compression cost in the query response `usage` block.

---

# 10. Interaction with the Context Manager

The Engine compresses **memory**; the [Context Manager](../04-agent-framework/context-manager.md)
assembles the **whole prompt** (system prompt, policies, conversation, retrieved
memory, tool results). The Engine guarantees its slice fits the requested
`token_budget`; the Context Manager owns the global budget across all prompt
sections (see
[Memory System §20](../04-agent-framework/memory-system.md#20-context-assembly)).

---

# 11. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Selection + dedup + pruning | < 8 ms p95 |
| Extractive summarization | < 20 ms p95 |
| Abstractive summarization | model-bound (cached when possible) |
| Summary cache hit ratio (stable corpora) | > 50% |

---

# 12. Dependencies

- [`06-memory-engine/ranking.md`](ranking.md)
- [`05-llm-gateway/token-management.md`](../05-llm-gateway/token-management.md)
- [`04-agent-framework/context-manager.md`](../04-agent-framework/context-manager.md)

---

# 13. Related Documents

- [`06-memory-engine/overview.md`](overview.md)
- [`06-memory-engine/retrieval.md`](retrieval.md)
- [`06-memory-engine/memory-api.md`](memory-api.md)

---

# 14. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Memory Engine Compression specification |
