<!--
File: docs/14-observability/metrics.md
Document ID: OBS-002
-->

# Observability: Metrics

**Document ID:** OBS-002  
**File Path:** `docs/14-observability/metrics.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Platform Operations Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines the **metric taxonomy** for the Apex AI Platform — what every service measures, the naming conventions, and the platform-specific cost/usage metrics.

---

# 2. Methodologies

- **RED** (request-driven services): Rate, Errors, Duration.
- **USE** (resources): Utilization, Saturation, Errors.
- **Cost** (AI-specific): tokens, spend, cache savings.

Every service exposes `/metrics` in Prometheus format
([deployment](../12-deployment/docker.md#6-health--ports)).

---

# 3. Naming Conventions

```text
apex_<subsystem>_<name>_<unit>
# examples
apex_api_requests_total{route,status}
apex_api_request_duration_seconds{route}        (histogram)
apex_workflow_executions_total{status}
apex_llm_tokens_total{model,type}
apex_llm_cost_usd_total{tenant,model}
apex_tool_queue_seconds{pool}                    (histogram)
apex_memory_retrieval_seconds{tier}             (histogram)
```

Labels are bounded (no unbounded cardinality like raw IDs); `tenant`/`project` are
included where roll-ups are needed.

---

# 4. Golden Signals (per service)

| Signal | Metric |
|--------|--------|
| Latency | `*_duration_seconds` histograms (p50/p95/p99) |
| Traffic | `*_requests_total` / `*_executions_total` |
| Errors | `*_errors_total` / error-status counters |
| Saturation | queue depth, pool utilization, memory/CPU |

These power the standard [dashboards](dashboards.md) and SLO
[alerts](alerting.md).

---

# 5. Subsystem Metrics

Each subsystem defines domain metrics, e.g.:

- LLM Gateway: tokens, cost, cache hit ratio, failover count
  ([token management](../05-llm-gateway/token-management.md#11-reporting))
- Tool Runtime: queue time, start latency, sandbox kills, warm-pool hit ratio
  ([observability-ops](../07-tool-runtime/observability-ops.md#3-key-metrics))
- Memory Engine: retrieval latency, recall proxy, index size, tier distribution
- Workflow Engine: execution duration, retries, compensation rate

---

# 6. Cost & Usage Metrics

Cost is a first-class signal, sourced from **cost events** on the Event Bus and
exposed as metrics:

```text
apex_llm_cost_usd_total{tenant,project,model}
apex_tool_executions_total{tenant,tool,status}
apex_memory_records{namespace,tier}
apex_cache_savings_usd_total{subsystem}
```

These feed the [cost explorer](../10-dashboard/monitoring.md#5-cost-explorer) and
[quota](../09-api/projects.md#5-quotas) utilization views.

---

# 7. Cardinality Management

- High-cardinality dimensions (run id, request id) live in **traces/logs**, not
  metric labels.
- Per-tenant metrics use bounded tenant labels; per-resource detail is via
  exemplars linking to traces.

---

# 8. Exemplars

Histograms carry **exemplars** (trace IDs) so a slow p99 bucket links directly to a
representative [trace](tracing.md) — closing the loop from "it's slow" to "here's
why."

---

# 9. SLIs → SLOs

Service-Level Indicators derived from these metrics back the SLOs in
[alerting.md](alerting.md) (e.g. Tool Runtime
[SLOs](../07-tool-runtime/observability-ops.md#6-service-level-objectives)).

---

# 10. Dependencies

- [`14-observability/tracing.md`](tracing.md)
- [`14-observability/dashboards.md`](dashboards.md)
- [`05-llm-gateway/token-management.md`](../05-llm-gateway/token-management.md)

---

# 11. Related Documents

- [`14-observability/alerting.md`](alerting.md)
- [`00-executive/success-metrics.md`](../00-executive/success-metrics.md)

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Metrics specification |
