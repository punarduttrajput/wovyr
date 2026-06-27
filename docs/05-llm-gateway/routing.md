<!--
File: docs/05-llm-gateway/routing.md
Document ID: LLM-003
-->

# LLM Gateway Routing

**Document ID:** LLM-003  
**File Path:** `docs/05-llm-gateway/routing.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines how the LLM Gateway selects a concrete **provider** and **model** for each request. Routing turns a capability-level request (e.g. "a chat model") into a specific dispatch (e.g. `anthropic / claude-opus-4-8`).

Routing builds on the [Provider SDK selection logic](../04-agent-framework/provider-sdk.md#10-provider-selection) but adds fleet-wide concerns: tenant policy, live health, budgets, and quotas.

---

# 2. Inputs to a Routing Decision

```text
Request model_selector
Tenant / project routing policy
Model registry (capabilities, pricing, context window)
Live provider health (from Resilience Engine)
Current budget / quota state (from Token Manager)
Region / data-residency constraints
```

---

# 3. Routing Modes

| Mode | Behavior |
|------|----------|
| `pinned` | Caller specifies `model`; Gateway uses it (or fails over if down) |
| `selector` | Caller specifies capability/class/strategy; Gateway chooses |
| `policy` | Tenant policy fully dictates the model; caller hints ignored |

Precedence: **policy** (if tenant enforces) → **pinned** → **selector**.

---

# 4. Selection Strategies

When in `selector` mode, the `strategy` field chooses how candidates are ranked:

| Strategy | Optimizes for |
|----------|---------------|
| `lowest_cost` | Cheapest model meeting the capability |
| `lowest_latency` | Fastest historical p95 |
| `highest_quality` | Highest configured quality score |
| `highest_availability` | Most healthy provider right now |
| `balanced` | Weighted blend of cost, latency, quality |
| `round_robin` | Even distribution across candidates |
| `sticky` | Same model for a session/conversation |

The default strategy is `balanced`. Weights are configurable per tenant.

---

# 5. Model Classes

`model_selector.class` lets callers request a tier without naming a model:

| Class | Intent |
|-------|--------|
| `frontier` | Most capable models |
| `balanced` | Good capability/cost tradeoff |
| `fast` | Low-latency, lower-cost models |
| `embedding` | Embedding-optimized models |
| `local` | On-prem / self-hosted models only |

Classes map to concrete models via the registry and tenant configuration, so a
class can be re-pointed to newer models without changing caller code.

---

# 6. Candidate Resolution Pipeline

```text
1. Filter by capability        (must support requested capability)
2. Filter by class             (if specified)
3. Filter by constraints       (region, residency, local-only, allow/deny list)
4. Filter by health            (drop providers with open circuit breakers)
5. Filter by budget            (drop models that would exceed budget)
6. Rank by strategy            (cost / latency / quality / balanced)
7. Select primary + ordered failover list
```

The output is a **primary choice plus an ordered failover list**, which the
[Resilience Engine](resilience.md) walks on failure.

---

# 7. Routing Policy (Per Tenant)

```yaml
routing:
  default_strategy: balanced
  weights:
    cost: 0.4
    latency: 0.4
    quality: 0.2
  class_map:
    frontier: [claude-opus-4-8, gpt-5]
    balanced: [claude-sonnet-4-6, gpt-5-mini]
    fast:     [claude-haiku-4-5, gpt-5-mini]
  allow_providers: [anthropic, openai, azure]
  deny_models: []
  region: us
  data_residency: strict
  local_only: false
```

Policies are validated and enforced via the
[Policy Engine](../04-agent-framework/policy-engine.md). A tenant may forbid
certain providers entirely (e.g. for data-residency reasons).

---

# 8. Sticky Routing

For multi-turn conversations, `sticky` routing keeps the same model across turns
to preserve behavior consistency. Stickiness is keyed by `conversation_id` and
stored in Redis with a TTL. If the sticky model becomes unhealthy, routing falls
back to the normal pipeline and updates the sticky binding.

---

# 9. Cost- and Budget-Aware Routing

Before ranking, the Router asks the [Token Manager](token-management.md) for the
remaining budget. Models whose **estimated** cost would exceed the remaining
budget are filtered out. If no model fits, the request is rejected with
`budget_exceeded` rather than silently downgraded — unless the tenant enables
`auto_downgrade`, in which case the cheapest viable model is selected.

---

# 10. Health-Aware Routing

The Router consults live health signals maintained by the
[Resilience Engine](resilience.md):

- Providers with an **open** circuit breaker are excluded.
- Providers in **half-open** state are deprioritized but eligible.
- Recent error rate and latency feed the `availability` and `latency` strategies.

---

# 11. Observability

Each routing decision records:

- Candidate set and the chosen primary
- Strategy and effective weights
- Reason codes for any exclusions (health, budget, residency)
- Number of failovers ultimately used

These appear in the response `routing` block (see
[Provider API §6](provider-api.md#6-chat-response-non-streaming)) and in traces.

---

# 12. Examples

**Cheapest chat model in-region:**

```json
{ "model_selector": { "capability": "chat", "strategy": "lowest_cost" } }
```

**Frontier class, pinned with failover:**

```json
{ "model": "claude-opus-4-8", "model_selector": { "class": "frontier" } }
```

**Local-only embeddings (data residency):**

```json
{ "model_selector": { "capability": "embeddings", "class": "local" } }
```

---

# 13. Dependencies

- [`04-agent-framework/provider-sdk.md`](../04-agent-framework/provider-sdk.md)
- [`05-llm-gateway/resilience.md`](resilience.md)
- [`05-llm-gateway/token-management.md`](token-management.md)
- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md)

---

# 14. Related Documents

- [`05-llm-gateway/overview.md`](overview.md)
- [`05-llm-gateway/provider-api.md`](provider-api.md)

---

# 15. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial LLM Gateway Routing specification |
