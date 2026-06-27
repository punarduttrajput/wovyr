<!--
File: docs/05-llm-gateway/token-management.md
Document ID: LLM-006
-->

# LLM Gateway Token Management & Cost

**Document ID:** LLM-006  
**File Path:** `docs/05-llm-gateway/token-management.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines how the LLM Gateway measures token usage, computes cost, attributes spend, and enforces budgets and quotas. Centralized accounting is a primary reason the Gateway exists: it gives the platform one trustworthy source of cost truth.

---

# 2. Responsibilities

- Count prompt, completion, cached, and reasoning tokens
- Compute cost from model registry pricing
- Attribute cost to tenant / organization / project / principal / agent
- Enforce per-request budgets
- Enforce rolling quotas (spend and rate)
- Emit cost events for downstream billing and analytics

---

# 3. Token Accounting Model

For every request the Gateway records:

```json
{
  "prompt_tokens": 412,
  "completion_tokens": 37,
  "cached_tokens": 128,
  "reasoning_tokens": 0,
  "total_tokens": 449,
  "billable_tokens": 321
}
```

| Field | Meaning |
|-------|---------|
| `prompt_tokens` | Input tokens sent to the model |
| `completion_tokens` | Output tokens generated |
| `cached_tokens` | Prompt tokens served from provider prompt cache |
| `reasoning_tokens` | Hidden reasoning tokens (where billed separately) |
| `total_tokens` | Sum of all counted tokens |
| `billable_tokens` | Tokens actually charged after cache discounts |

Token counts come from the provider response when available; otherwise the
Gateway estimates using the model tokenizer from the
[Provider SDK](../04-agent-framework/provider-sdk.md#17-token-management).

---

# 4. Cost Computation

Cost is derived from the model registry pricing:

```text
cost = (prompt_tokens   / 1000) * input_per_1k
     + (completion_tokens/ 1000) * output_per_1k
     + (cached_tokens    / 1000) * cached_input_per_1k
     + image/audio unit charges (if applicable)
```

Pricing is versioned in the registry; the **pricing version** in effect is stored
with each cost record so historical costs remain reproducible after price changes.

---

# 5. Cost Attribution

Every cost record is tagged with the full attribution chain:

```json
{
  "tenant": "acme",
  "organization": "acme-eu",
  "project": "support-bot",
  "principal": "svc-agent-runtime",
  "agent": "order-assistant",
  "model": "claude-opus-4-8",
  "provider": "anthropic",
  "cost_usd": 0.0061,
  "pricing_version": "2026-06-01"
}
```

This enables cost roll-ups along any dimension (tenant, project, agent, model).

---

# 6. Budgets (Per Request)

A request may carry a `budget` block (see
[Provider API §4](provider-api.md#4-common-request-envelope)):

```json
{ "budget": { "max_cost_usd": 0.50, "max_tokens": 4096 } }
```

Enforcement:

1. **Pre-check** — before dispatch, the Gateway estimates worst-case cost
   (`prompt_tokens + max_tokens`). If it exceeds the budget, the request is
   rejected with `budget_exceeded` (unless `auto_downgrade` is enabled — see
   [Routing §9](routing.md#9-cost--and-budget-aware-routing)).
2. **In-flight** — for streaming, the Gateway tracks accumulating cost and aborts
   the stream if `max_cost_usd` is reached, emitting a final `usage` event.

---

# 7. Quotas (Rolling)

Quotas are enforced per scope over time windows, with state in Redis so they hold
across Gateway instances.

```yaml
quotas:
  - scope: project
    id: support-bot
    limits:
      requests_per_minute: 600
      tokens_per_day: 5_000_000
      cost_per_day_usd: 250
  - scope: tenant
    id: acme
    limits:
      cost_per_month_usd: 10000
```

Behavior on breach:

| Limit | Response |
|-------|----------|
| Rate (rpm) | `429 quota_exceeded` with `Retry-After` |
| Token/day | `429 quota_exceeded` |
| Cost/day or /month | `402 budget_exceeded` |

Soft thresholds (e.g. 80%) emit **alerts** without blocking.

---

# 8. Rate Limiting

Rate limits apply at provider, tenant, organization, project, agent, and user
scopes (aligned with the [Provider SDK §19](../04-agent-framework/provider-sdk.md#19-rate-limiting)).
The Gateway uses a token-bucket per scope; the most restrictive applicable limit
wins. Provider-side `429`s also feed back to slow the corresponding bucket.

---

# 9. Cost Events

After each request the Gateway publishes a cost event to the
[Event Bus](../03-workflow-engine/event-bus.md):

```json
{
  "event": "llm.usage.recorded",
  "request_id": "req_01H...",
  "attribution": { "tenant": "acme", "project": "support-bot", "agent": "order-assistant" },
  "usage": { "total_tokens": 449, "billable_tokens": 321, "cost_usd": 0.0061 },
  "model": "claude-opus-4-8",
  "cache": "miss",
  "timestamp": "2026-06-27T10:00:00Z"
}
```

Downstream consumers: billing, the dashboard cost explorer, and
[Success Metrics KPIs](../00-executive/success-metrics.md). Events are emitted
at-least-once; consumers deduplicate by `request_id`.

---

# 10. Cost Optimization Hooks

The Gateway supports (configurable per tenant):

| Technique | Effect |
|-----------|--------|
| Response caching | Avoids spend on repeats — see [Caching](caching.md) |
| Prompt cache awareness | Credits `cached_tokens` at reduced rate |
| Auto-downgrade | Routes to cheaper model when budget-constrained |
| Max-token clamping | Caps `max_tokens` to tenant ceiling |
| Batch embeddings | Reduces per-call overhead |

---

# 11. Reporting

Aggregations exposed via metrics and the dashboard:

- Cost by tenant / project / agent / model over time
- Tokens by type (prompt / completion / cached)
- Cache savings (estimated cost avoided)
- Budget/quota utilization and breach counts

---

# 12. Non-Functional Targets

| Metric | Target |
|--------|--------|
| Token accounting overhead | < 1 ms |
| Budget pre-check | < 2 ms |
| Quota check (Redis) | < 3 ms |
| Cost event emission | asynchronous, non-blocking |

---

# 13. Failure Behavior

If the Token Manager or its Redis backend is unreachable, budget/quota checks
**fail closed by default** (requests rejected) to prevent uncontrolled spend.
Tenants may opt into fail-open for availability-critical workloads.

---

# 14. Dependencies

- [`04-agent-framework/provider-sdk.md`](../04-agent-framework/provider-sdk.md#17-token-management)
- [`03-workflow-engine/event-bus.md`](../03-workflow-engine/event-bus.md)
- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md)

---

# 15. Related Documents

- [`05-llm-gateway/overview.md`](overview.md)
- [`05-llm-gateway/routing.md`](routing.md)
- [`05-llm-gateway/caching.md`](caching.md)
- [`00-executive/success-metrics.md`](../00-executive/success-metrics.md)

---

# 16. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial LLM Gateway Token Management & Cost specification |
