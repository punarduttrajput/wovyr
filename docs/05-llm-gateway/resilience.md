<!--
File: docs/05-llm-gateway/resilience.md
Document ID: LLM-004
-->

# LLM Gateway Resilience & Failover

**Document ID:** LLM-004  
**File Path:** `docs/05-llm-gateway/resilience.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines how the LLM Gateway stays available when individual model providers are slow, rate-limited, or down. It specifies timeouts, retries, failover, circuit breaking, and error normalization.

The goal: a transient provider failure should never surface to the caller as long as a healthy alternative exists.

---

# 2. Resilience Layers

```text
Request
   │
   ▼
Timeout Guard ──► bounds every attempt
   │
   ▼
Retry Policy  ──► retries the same provider on transient errors
   │
   ▼
Failover      ──► moves to the next provider in the candidate list
   │
   ▼
Circuit Break ──► removes unhealthy providers from routing
   │
   ▼
Error Mapper  ──► normalizes the final error to the public contract
```

---

# 3. Timeouts

| Timeout | Default | Scope |
|---------|---------|-------|
| `connect_timeout` | 2 s | TCP/TLS establishment |
| `first_token_timeout` | 20 s | Time to first streamed token |
| `request_timeout` | 120 s | Whole non-streaming request |
| `idle_stream_timeout` | 30 s | Max gap between stream events |

Timeouts are configurable per provider and may be overridden per request within
tenant-allowed bounds. A timeout is a **retryable** condition.

---

# 4. Retry Policy

Retries apply only to **transient** failures.

```yaml
retry:
  max_attempts: 3
  strategy: exponential
  base_delay: 200ms
  max_delay: 4s
  jitter: full
  retry_on:
    - timeout
    - provider_5xx
    - connection_error
    - provider_rate_limited   # honors Retry-After when present
  do_not_retry_on:
    - invalid_request
    - unauthenticated
    - forbidden
    - budget_exceeded
```

Rules:

- Retries within the same provider count toward `max_attempts`.
- When a provider returns `429` with `Retry-After`, that value overrides backoff.
- Non-idempotent streaming requests are retried only before the first token is
  emitted; once tokens have streamed, the request fails over instead of retrying.

---

# 5. Failover

When retries against the primary provider are exhausted (or the provider is
circuit-broken), the Gateway advances to the next entry in the ordered candidate
list produced by [Routing](routing.md).

```text
claude-opus-4-8 (anthropic)  ─ retries exhausted ─►
gpt-5 (openai)               ─ rate limited       ─►
claude-sonnet-4-6 (anthropic)─ success
```

Failover rules:

- The candidate list is capped (`max_failovers`, default 2) to bound latency.
- Each failover hop is recorded in the response `routing.failovers` count.
- Failover respects budget: a more expensive fallback is skipped if it would
  exceed the request budget.
- If the caller pinned a `model` with no `model_selector`, failover is limited to
  other deployments of the same model (e.g. Azure OpenAI ↔ OpenAI) unless the
  tenant allows cross-model failover.

---

# 6. Circuit Breaker

Each provider (optionally per model) has a circuit breaker to stop hammering an
unhealthy upstream.

| State | Meaning | Behavior |
|-------|---------|----------|
| Closed | Healthy | Requests flow normally |
| Open | Unhealthy | Provider excluded from routing |
| Half-Open | Probing | Limited trial requests allowed |

```yaml
circuit_breaker:
  error_threshold: 0.5        # fraction of failures in window
  window: 30s
  min_requests: 20            # minimum volume before tripping
  open_duration: 15s          # cool-down before half-open
  half_open_max_calls: 5
```

State is shared across Gateway instances via Redis so the whole fleet reacts to a
failing provider consistently. The breaker state feeds back into
[health-aware routing](routing.md#10-health-aware-routing).

---

# 7. Hedging (Optional)

For latency-sensitive tenants, the Gateway can issue a **hedged** request: after
a configurable delay with no first token, it dispatches the same request to the
next candidate and returns whichever responds first, cancelling the loser.

```yaml
hedging:
  enabled: false
  delay: 800ms
  max_parallel: 2
```

Hedging increases cost and is disabled by default; it is metered as separate
attempts.

---

# 8. Error Normalization

Raw provider errors are mapped to the public contract codes (see
[Provider API §10](provider-api.md#10-error-model)).

| Raw provider signal | Normalized code | Retryable |
|---------------------|-----------------|-----------|
| HTTP 408 / socket timeout | `timeout` | yes |
| HTTP 429 | `provider_rate_limited` | yes |
| HTTP 500/502/503/504 | `provider_unavailable` | yes |
| HTTP 400 (bad params) | `invalid_request` | no |
| HTTP 401/403 (auth) | `unauthenticated` / `forbidden` | no |
| Content filtered by provider | `invalid_request` (with detail) | no |

After all retries and failovers are exhausted, the **last** normalized error is
returned to the caller, annotated with the providers attempted.

---

# 9. Degraded Modes

| Condition | Degraded behavior |
|-----------|-------------------|
| Cache backend down | Bypass cache; serve live |
| All providers for a class down | Try other allowed classes if tenant permits |
| Telemetry sink down | Buffer and continue serving |
| Token Manager unreachable | Fail closed on budgets (reject) by default; configurable |

The budget behavior defaults to **fail-closed** to prevent uncontrolled spend.

---

# 10. Observability

Resilience emits metrics for:

- Retry count per provider
- Failover count and depth
- Circuit breaker state transitions
- Timeout occurrences
- Final error codes by type

Alerts fire on sustained open circuits or elevated failover rates.

---

# 11. Non-Functional Targets

| Metric | Target |
|--------|--------|
| Failover decision overhead | < 10 ms |
| Circuit-breaker state read | < 2 ms |
| Successful failover (caller-visible success despite primary down) | > 99% when any healthy candidate exists |

---

# 12. Dependencies

- [`05-llm-gateway/routing.md`](routing.md)
- [`05-llm-gateway/token-management.md`](token-management.md)
- [`05-llm-gateway/streaming.md`](streaming.md)
- [`04-agent-framework/provider-sdk.md`](../04-agent-framework/provider-sdk.md#11-automatic-failover)

---

# 13. Related Documents

- [`05-llm-gateway/overview.md`](overview.md)
- [`05-llm-gateway/provider-api.md`](provider-api.md)
- [`03-workflow-engine/retry-engine.md`](../03-workflow-engine/retry-engine.md)

---

# 14. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial LLM Gateway Resilience & Failover specification |
