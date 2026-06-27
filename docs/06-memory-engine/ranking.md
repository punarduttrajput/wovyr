<!--
File: docs/06-memory-engine/ranking.md
Document ID: MEM-005
-->

# Memory Engine Ranking

**Document ID:** MEM-005  
**File Path:** `docs/06-memory-engine/ranking.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines how the Memory Engine **orders** the candidate set produced by [Retrieval](retrieval.md). Ranking decides which memories are most worth spending scarce context tokens on before [Compression](compression.md) trims to the budget.

Good retrieval maximizes recall; good ranking maximizes the *usefulness* of what reaches the model.

---

# 2. Scoring Signals

| Signal | Meaning | Source |
|--------|---------|--------|
| `relevance` | Semantic/keyword match to the query | Fusion score from retrieval |
| `recency` | How recent the memory is | `created_at` / `updated_at` |
| `importance` | Intrinsic value of the memory | Stored `importance` field |
| `frequency` | How often it has been useful | Access/usefulness counters |
| `proximity` | Graph distance from query entities | Knowledge graph hops |
| `confidence` | Source trust / verification | `labels.confidence` |

---

# 3. Composite Score

The default ranking is a weighted sum of normalized signals:

```text
score = w_rel * relevance
      + w_rec * recency_decay(age)
      + w_imp * importance
      + w_frq * frequency_norm
      + w_prx * proximity_decay(hops)
```

Default weights (tenant- and query-overridable via
[`ranking.weights`](memory-api.md#6-query-request)):

```yaml
weights:
  relevance:  0.55
  recency:    0.20
  importance: 0.15
  frequency:  0.05
  proximity:  0.05
```

All signals are normalized to `[0,1]` before weighting so no single raw scale
dominates.

---

# 4. Recency Decay

Recency uses exponential decay so fresh memories are favored without erasing
durable knowledge:

```text
recency_decay(age) = exp(-age / half_life)
```

Half-life is configurable **per memory type**, reflecting how fast each kind of
memory goes stale:

| Type | Half-life (default) |
|------|---------------------|
| Conversation | 2 days |
| Workflow | 14 days |
| Episodic | 90 days |
| Semantic / Organizational | ∞ (no decay) |

Semantic facts (policies, docs) intentionally do not decay.

---

# 5. Importance

`importance` is a stored `[0,1]` value set by:

- Explicit caller assignment at write time
- Heuristics (e.g. user-pinned, approval decisions, error post-mortems)
- Background scoring (e.g. memories frequently retrieved and used)

Importance lets a highly relevant-but-trivial match lose to a slightly-less-relevant-but-critical one.

---

# 6. Frequency & Usefulness Feedback

The Engine tracks how often a retrieved memory is actually **used** (the caller
can report usage via the [Event Bus](../03-workflow-engine/event-bus.md)). A
memory repeatedly retrieved but never used is down-weighted over time; one
consistently used is boosted. This creates a slow feedback loop toward genuinely
useful memories.

---

# 7. Diversity & De-duplication

Before returning, the ranker applies **Maximal Marginal Relevance (MMR)** to avoid
returning N near-duplicates of the same fact:

```text
MMR = λ * relevance(d) - (1-λ) * max_similarity(d, already_selected)
```

This trades a little relevance for coverage, so the result set spans distinct
facts. Exact/near-duplicate collapsing is also handled here (and again in
[Compression](compression.md)).

---

# 8. Policy Filtering

After scoring, an ABAC policy pass (via the
[Policy Engine](../04-agent-framework/policy-engine.md)) drops any record the
principal may not see based on **content-derived** attributes (e.g. a memory
tagged `pii:true` for a principal lacking PII scope). Scope-level filtering already
happened earlier in [Retrieval §5](retrieval.md#5-scope--permission-filtering).

---

# 9. Output

The ranker returns each result with a `score` and a `score_breakdown` so callers
and operators can see *why* something ranked where it did (see
[Memory API §7](memory-api.md#7-query-response)). This transparency is required
for auditability and tuning.

---

# 10. Tuning & Evaluation

- Weights and half-lives are configurable per tenant and overridable per query.
- The Engine logs ranking inputs so offline evaluation (NDCG, MRR against
  labeled relevance sets) can tune defaults.
- A/B weight profiles can be assigned per project to compare ranking quality.

---

# 11. Determinism

For a fixed corpus version and weight profile, ranking is deterministic. Feedback
signals (frequency) are versioned by snapshot so a replayed query reproduces the
historical ordering.

---

# 12. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Ranking over 100 candidates | < 5 ms p95 |
| MMR diversification | < 3 ms |
| Policy filter pass | < 4 ms |

---

# 13. Dependencies

- [`06-memory-engine/retrieval.md`](retrieval.md)
- [`06-memory-engine/compression.md`](compression.md)
- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md)
- [`03-workflow-engine/event-bus.md`](../03-workflow-engine/event-bus.md)

---

# 14. Related Documents

- [`06-memory-engine/overview.md`](overview.md)
- [`06-memory-engine/memory-api.md`](memory-api.md)

---

# 15. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Memory Engine Ranking specification |
