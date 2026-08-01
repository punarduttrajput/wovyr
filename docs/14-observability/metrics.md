<!--
File: docs/14-observability/metrics.md
Document ID: OBS-002
-->

# Observability: Metrics

**Document ID:** OBS-002  
**File Path:** `docs/14-observability/metrics.md`  
**Version:** 1.1.0  
**Status:** Draft (aspirational multi-service taxonomy) — **§3.1 is the real,
CI-verifiable inventory** of what the single-node server emits today. Everything
outside §3.1 describes a future multi-service fleet; treat it as design intent,
not as series you can query. Same split as
[alerting.md](alerting.md)/[dashboards.md](dashboards.md), whose working starter
artifacts live in
[`deployment/observability/`](../../deployment/observability/README.md).  
**Owner:** Platform Operations Team  
**Last Updated:** 2026-08-01

---

# 1. Purpose

This document defines the **metric taxonomy** for the Wovyr AI Platform — what every service measures, the naming conventions, and the platform-specific cost/usage metrics.

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
wovyr_<subsystem>_<name>_<unit>
```

Labels are bounded (no unbounded cardinality like raw IDs); `tenant`/`project` are
included where roll-ups are needed.

## 3.1 What the platform emits today

The complete set of series exposed by `GET /metrics` on the single-node server.
Nothing else in this document is currently queryable.

| Metric | Type | Labels | Emitted by |
|---|---|---|---|
| `wovyr_api_requests_total` | counter | `route`, `method`, `status` | `hardening::track_metrics` |
| `wovyr_api_request_duration_seconds` | histogram | `route`, `method` | `hardening::track_metrics` |
| `wovyr_api_requests_by_tenant_total` | counter | `tenant`, `status_class` | `hardening::track_metrics` |
| `wovyr_llm_tokens_total` | counter | `model`, `type` (`prompt`/`completion`) | `config::MetricsCostObserver` |
| `wovyr_llm_cost_usd_total` | counter | `model` | `config::MetricsCostObserver` |
| `wovyr_llm_cost_usd_by_tenant_total` | counter | `tenant`, `project` | `hardening::record_llm_usage_metrics` |
| `wovyr_llm_tokens_by_tenant_total` | counter | `tenant`, `project` | `hardening::record_llm_usage_metrics` |
| `wovyr_cache_savings_usd_total` | counter | `subsystem` | `config::MetricsCostObserver` |
| `wovyr_webhook_deliveries_total` | counter | `result` (`delivered`/`failed`) | `webhooks.rs` |
| `wovyr_async_runs_in_flight` | gauge | — | `refresh_operability_gauges` |
| `wovyr_quota_runs_in_flight` | gauge | — | `refresh_operability_gauges` |
| `wovyr_workflow_executions_active` | gauge | — | `refresh_operability_gauges` |
| `wovyr_workflow_timers_pending` | gauge | — | `refresh_operability_gauges` |
| `wovyr_webhook_outbox_pending` | gauge | — | `refresh_operability_gauges` |
| `wovyr_webhook_dlq_size` | gauge | — | `refresh_operability_gauges` |

Two label-design notes worth carrying into any new metric:

- **Tenant is deliberately a separate series, not a label on the RED metrics**
  (RM-AIM-P2 OBS-201). Adding `tenant` to `wovyr_api_requests_total` would
  multiply an already `route × method × status` count by the tenant count, so
  per-tenant traffic lives in its own low-cardinality aggregate keyed by a coarse
  `status_class` instead. Tenant/project label values are bounded by a shared cap
  (first 200 distinct values keep their name; the rest fold into `other`).
- **The six gauges are recomputed from the durable stores at every scrape**
  (RM-AIM-P3 OBS-301) rather than maintained by inc/dec bookkeeping, so they
  survive a restart and cannot drift.

Not yet emitted, despite appearing in the taxonomy below:
`wovyr_workflow_executions_total`, `wovyr_tool_queue_seconds`,
`wovyr_memory_retrieval_seconds`, `wovyr_tool_executions_total`,
`wovyr_memory_records`.

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

# 5. Subsystem Metrics *(target state)*

Each subsystem defines domain metrics, e.g.:

- LLM Gateway: tokens, cost, cache hit ratio, failover count
  ([token management](../05-llm-gateway/token-management.md#11-reporting))
- Tool Runtime: queue time, start latency, sandbox kills, warm-pool hit ratio
  ([observability-ops](../07-tool-runtime/observability-ops.md#3-key-metrics))
- Memory Engine: retrieval latency, recall proxy, index size, tier distribution
- Workflow Engine: execution duration, retries, compensation rate

---

# 6. Cost & Usage Metrics

Cost is a first-class signal. Today it is sourced from the gateway's
`CostObserver` (in-process, not an event bus — there is no NATS deployment, see
[ADR-0005](../17-adr/ADR-0005-nats.md)) and from the run-path accounting sites
that already resolve a run's usage against a project quota:

```text
wovyr_llm_cost_usd_total{model}                       # gateway-wide
wovyr_llm_tokens_total{model,type}
wovyr_llm_cost_usd_by_tenant_total{tenant,project}    # run-path attributed
wovyr_llm_tokens_by_tenant_total{tenant,project}
wovyr_cache_savings_usd_total{subsystem}
```

Note the split: the gateway-wide counters carry `model` only, because a
`CostObserver` is attached once to the shared `Gateway` with no per-request
tenant context. Per-tenant attribution is a separate call made where tenant,
project, and `Usage` are all already in scope.

*Target state, not emitted:* `wovyr_tool_executions_total{tenant,tool,status}`
and `wovyr_memory_records{namespace,tier}`.

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
- [`deployment/observability/README.md`](../../deployment/observability/README.md) — the working Prometheus rules + Grafana dashboard built on §3.1's series

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.1.0 | 2026-08-01 | Added §3.1, the real emitted-series inventory (15 metrics, with labels and emitting call site), and marked the rest of the document as target-state. The prior version listed five metrics that are not emitted and omitted ten that are — including all six OBS-301 operability gauges — and gave `wovyr_api_requests_total`/`wovyr_llm_cost_usd_total` label sets that never matched the code. Corrected §6's Event-Bus sourcing claim (cost comes from an in-process `CostObserver`; there is no event bus deployment) |
| 1.0.0 | 2026-06-27 | Initial Metrics specification |
