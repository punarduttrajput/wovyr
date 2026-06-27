<!--
File: docs/14-observability/tracing.md
Document ID: OBS-003
-->

# Observability: Tracing

**Document ID:** OBS-003  
**File Path:** `docs/14-observability/tracing.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Platform Operations Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines **distributed tracing** across the Apex AI Platform — how a single request is traced end to end through services, and how traces correlate with logs and metrics.

---

# 2. Standard

Tracing uses **OpenTelemetry** with W3C Trace Context propagation. Every service is
instrumented to create and propagate spans, exporting via OTLP to a tracing backend
(Tempo/Jaeger).

---

# 3. End-to-End Trace

A user request produces one trace spanning every hop:

```text
trace: agent.run
 ├─ api-gateway: authn/authz
 ├─ agent-runtime: plan
 │   ├─ memory-engine: retrieve  (vector + rank)
 │   ├─ llm-gateway: chat        (route → provider)
 │   └─ tool-runtime: execute    (sandbox → tool)
 └─ agent-runtime: respond
```

This makes "why was this run slow/expensive?" answerable by inspecting span
durations and attributes.

---

# 4. Propagation

```text
Client → API Gateway   (generates trace_id + request_id)
       → service A → service B → datastores
```

Context flows via headers on REST/gRPC and metadata on
[Event Bus](../02-architecture/event-driven-architecture.md) messages, so async
work (workflow steps, cost events) joins the same trace where applicable.

---

# 5. Span Conventions

| Attribute | Example |
|-----------|---------|
| `apex.tenant` | `acme` |
| `apex.principal` | `agent:order-assistant` |
| `apex.resource.id` | `run_01H...` |
| `apex.subsystem` | `llm-gateway` |
| Semantic conventions | `http.*`, `rpc.*`, `db.*` |

Sensitive values are never placed in span attributes (same masking rules as
[logging](logging.md#6-privacy--security)).

---

# 6. Correlation Across Pillars

- `trace_id` appears in every [log](logging.md#5-correlation) line.
- [Metrics](metrics.md#8-exemplars) histograms carry trace exemplars.
- The [dashboard](../10-dashboard/monitoring.md#8-drill-down--correlation) links a
  metric/alert → trace → logs using the shared IDs.

---

# 7. Domain Spans

Subsystems emit meaningful spans, e.g.:

- LLM Gateway: routing decision, provider call, failover hops
  ([routing observability](../05-llm-gateway/routing.md#11-observability))
- Tool Runtime: dispatch → authorize → schedule → sandbox → execute
  ([tracing](../07-tool-runtime/observability-ops.md#4-tracing))
- Memory Engine: embed → search → rank → compress

---

# 8. Sampling

- **Head-based** sampling by default (configurable rate) to bound volume.
- **Tail-based** sampling can retain all error/slow traces regardless of rate.
- Errors and high-latency requests are always sampled.

---

# 9. Cost Attribution via Traces

Because model and tool calls are spans with cost attributes, a trace shows the
**cost breakdown** of a single request, complementing aggregate
[cost metrics](metrics.md#6-cost--usage-metrics).

---

# 10. Dependencies

- [`14-observability/logging.md`](logging.md)
- [`14-observability/metrics.md`](metrics.md)
- [`02-architecture/event-driven-architecture.md`](../02-architecture/event-driven-architecture.md)

---

# 11. Related Documents

- [`14-observability/dashboards.md`](dashboards.md)
- [`10-dashboard/monitoring.md`](../10-dashboard/monitoring.md)

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Tracing specification |
