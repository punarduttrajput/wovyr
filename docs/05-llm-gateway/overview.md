<!--
File: docs/05-llm-gateway/overview.md
Document ID: LLM-001
-->

# LLM Gateway Overview

**Document ID:** LLM-001  
**File Path:** `docs/05-llm-gateway/overview.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document specifies the **LLM Gateway**, the deployable service that fronts every AI model provider used by the Wovyr AI Platform.

The Gateway provides a single, governed, provider-neutral endpoint for model inference. It centralizes everything that should not be re-implemented inside each calling service: credentials, routing, failover, caching, rate limiting, token accounting, cost control, and observability.

---

# 2. Scope

The LLM Gateway is responsible for:

- A network API for inference (chat, completion, embeddings, image, moderation)
- Provider and model selection (routing)
- Resilience: retries, failover, timeouts, circuit breaking
- Streaming responses to callers
- Token accounting and cost attribution
- Budget and quota enforcement
- Response caching
- Centralized credential management
- Per-request audit and telemetry

The LLM Gateway is **not** responsible for:

- Defining provider adapters — see [Provider SDK](../04-agent-framework/provider-sdk.md)
- Prompt construction — see [Context Manager](../04-agent-framework/context-manager.md)
- Deciding *what* to ask a model — that is the Agent Runtime's job

---

# 3. Position in the Platform

```text
 Agent Runtime ─┐
 Workflow Engine├──► LLM Gateway ──► Provider SDK ──► Provider APIs
 Tool Runtime   │        │
 Dashboard      ┘        ├── Cache (Redis)
                         ├── Credential Vault
                         └── Telemetry → Event Bus / Prometheus
```

The Gateway is a horizontally scalable, stateless-per-request service. Shared
state (cache entries, quota counters, circuit-breaker status) lives in Redis so
any instance can serve any request. See
[C4 Container §4.5](../02-architecture/c4-container.md).

---

# 4. Responsibilities

## 4.1 Provider Abstraction

The Gateway exposes one request schema for all providers. Callers select a
*capability* and optionally a *model class*; the Gateway resolves the concrete
provider and model. Provider-specific payloads never leak to callers.

## 4.2 Routing

The Router selects a provider/model based on explicit request, capability match,
cost, latency, availability, region, and tenant preference. See [Routing](routing.md).

## 4.3 Resilience

The Resilience Engine applies timeouts, bounded retries with backoff, failover to
alternate providers, and circuit breaking for unhealthy providers. See
[Resilience & Failover](resilience.md).

## 4.4 Streaming

Token, tool-call, and progress events are delivered over a single unified
streaming protocol independent of provider wire format. See [Streaming](streaming.md).

## 4.5 Token & Cost Management

Every request is metered. Prompt, completion, and cached tokens are recorded;
cost is computed from the model registry pricing; budgets and quotas are enforced.
See [Token Management & Cost](token-management.md).

## 4.6 Caching

Exact-match and semantic caching reduce cost and latency for repeated or similar
requests. See [Caching](caching.md).

## 4.7 Governance

Each request carries a tenant, organization, project, and principal. The Gateway
authenticates the caller, applies [Policy Engine](../04-agent-framework/policy-engine.md)
rules, enforces quotas, and writes an audit record.

---

# 5. Supported Capabilities

| Capability | Description |
|------------|-------------|
| chat | Multi-turn chat completion |
| completion | Single-prompt text completion |
| embeddings | Vector embeddings |
| function_calling | Tool/function calling |
| structured_output | JSON / JSON-Schema constrained output |
| vision | Image input understanding |
| image_generation | Image output |
| audio | Speech-to-text / text-to-speech |
| moderation | Content safety classification |

Supported providers are defined by the [Provider SDK](../04-agent-framework/provider-sdk.md#5-supported-providers).

---

# 6. Request Lifecycle

```text
1.  Receive request            (REST / gRPC / WebSocket)
2.  Authenticate caller        (JWT / mTLS / service token)
3.  Resolve tenant + principal
4.  Apply policy checks        (Policy Engine)
5.  Pre-check budget + quota   (Token Manager)
6.  Cache lookup               (exact, then semantic)
        ├── hit  → record usage(cached) → return
        └── miss → continue
7.  Route                      (select provider + model)
8.  Execute with resilience    (retry / failover / circuit breaker)
9.  Stream or collect response (Provider SDK → provider API)
10. Meter usage + compute cost
11. Emit cost event + telemetry
12. Store in cache (if cacheable)
13. Return response + usage metadata
```

---

# 7. Deployment Modes

| Mode | Description |
|------|-------------|
| Embedded | Gateway runs in-process within the all-in-one dev binary |
| Sidecar | Co-located with the Agent Runtime for low-latency calls |
| Standalone | Dedicated horizontally scaled service (enterprise default) |

In all modes the API contract is identical. See
[Deployment Architecture](../02-architecture/deployment-architecture.md).

---

# 8. Module Organization

```text
service-llm-gateway/
├── api/            # REST + gRPC + WebSocket handlers
├── router/         # provider & model selection
├── resilience/     # retry, failover, circuit breaker
├── streaming/      # unified streaming engine
├── tokens/         # accounting, budgets, cost
├── cache/          # exact + semantic cache
├── credentials/    # secret references, rotation
├── telemetry/      # logs, metrics, traces, cost events
└── main.rs
```

The `router`, `resilience`, and `streaming` modules call into the
[Provider SDK](../04-agent-framework/provider-sdk.md) rather than provider APIs directly.

---

# 9. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Gateway overhead (non-cached) | < 8 ms p95 |
| Cache lookup | < 3 ms p95 |
| Routing decision | < 5 ms p95 |
| Failover decision | < 10 ms |
| Streaming first-token added latency | < 15 ms |
| Availability | 99.99% |
| Throughput | 10k+ concurrent in-flight requests per instance |

---

# 10. Failure Handling Summary

| Failure | Behavior |
|---------|----------|
| Provider timeout | Retry, then failover to next provider |
| Provider 5xx | Retry with backoff, then failover |
| Provider rate limit (429) | Backoff, optionally reroute |
| All providers down | Return `503` with `retry-after` |
| Budget exceeded | Reject with `402`-style budget error |
| Quota exceeded | Reject with `429` quota error |
| Cache backend down | Bypass cache, serve live (degraded) |

Details in [Resilience & Failover](resilience.md).

---

# 11. Security

- Provider credentials are stored as secret references and never returned to callers.
- All caller traffic requires authentication (JWT, service token, or mTLS).
- Inter-service traffic uses mTLS.
- Requests and responses may be PII-masked before logging.
- Every request produces a structured audit record (tenant, principal, model, tokens, cost).

See [Policy Engine](../04-agent-framework/policy-engine.md) and the planned
`13-security/` section.

---

# 12. Observability

Every request emits:

- **Logs** — structured, correlation-ID tagged
- **Metrics** — latency, tokens, cost, cache hit ratio, failover count, error rate
- **Traces** — OpenTelemetry spans across routing, provider call, and streaming
- **Cost events** — published to the [Event Bus](../03-workflow-engine/event-bus.md)

KPIs are tracked in [Success Metrics — LLM Gateway KPIs](../00-executive/success-metrics.md).

---

# 13. Dependencies

- [`04-agent-framework/provider-sdk.md`](../04-agent-framework/provider-sdk.md)
- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md)
- [`03-workflow-engine/event-bus.md`](../03-workflow-engine/event-bus.md)

---

# 14. Related Documents

- [`05-llm-gateway/provider-api.md`](provider-api.md)
- [`05-llm-gateway/routing.md`](routing.md)
- [`05-llm-gateway/resilience.md`](resilience.md)
- [`05-llm-gateway/streaming.md`](streaming.md)
- [`05-llm-gateway/token-management.md`](token-management.md)
- [`05-llm-gateway/caching.md`](caching.md)

---

# 15. Future Enhancements

- Quality-aware routing using live model scoring
- Multi-model ensemble and speculative routing
- Prompt registry integration
- MCP gateway integration
- Edge / regional inference pools
- Automatic prompt compression before dispatch

---

# 16. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial LLM Gateway Overview |
