<!--
File: docs/14-observability/dashboards.md
Document ID: OBS-004
-->

# Observability: Dashboards

**Document ID:** OBS-004  
**File Path:** `docs/14-observability/dashboards.md`  
**Version:** 1.1.0  
**Status:** Draft (aspirational multi-service catalog) — a **real, working starter**
dashboard now exists at [`deployment/observability/dashboard.json`](../../deployment/observability/dashboard.json)
(RM-GA-P4 OBS-803): RED-per-route + LLM cost/token panels over the actual metrics
the single-node server emits, not the full 7-dashboard catalog §2 describes. Import
it into a Grafana instance directly; see that directory's `README.md` for scope and
caveats (never rendered against a live Grafana in this dev environment).  
**Owner:** Platform Operations Team  
**Last Updated:** 2026-07-09

---

# 1. Purpose

This document defines the **standard dashboards** for operating the Wovyr AI Platform — the curated views over [metrics](metrics.md), [traces](tracing.md), and [logs](logging.md) that operators rely on.

These power both Grafana and the native
[dashboard monitoring](../10-dashboard/monitoring.md) surfaces.

---

# 2. Dashboard Catalog

| Dashboard | Audience | Shows |
|-----------|----------|-------|
| Platform Health | On-call | Service up/down, golden signals, SLOs |
| Service Detail | Owners | Per-service RED/USE deep dive |
| Workflow & Agent | Operators | Executions/runs, durations, failures |
| LLM Cost & Usage | FinOps/admins | Tokens, spend, cache savings, by model/tenant |
| Tool Runtime | Operators | Queue, start latency, sandbox kills, pools |
| Memory Engine | Operators | Retrieval latency, index size, tiers |
| Capacity | Operators | Saturation, autoscaling, headroom |

---

# 3. Golden-Signal Layout

Each service dashboard follows a consistent layout:

```text
┌── Latency (p50/p95/p99) ──┬── Traffic (req/s) ──┐
├── Errors (rate, by code) ─┴── Saturation (CPU/mem/queue) ─┤
└── Exemplars → traces · recent error logs ────────────────┘
```

Panels link to [traces](tracing.md#6-correlation-across-pillars) via exemplars and
to filtered logs, so an operator drills from a spike to root cause in a click.

---

# 4. Cost Dashboard

Built on [cost metrics/events](metrics.md#6-cost--usage-metrics):

- Spend over time by tenant / project / agent / model
- Token mix (prompt/completion/cached)
- Cache savings ([LLM](../05-llm-gateway/caching.md) + [Memory](../06-memory-engine/compression.md))
- Budget/quota utilization vs. [project quotas](../09-api/projects.md#5-quotas)

This is the operator-facing twin of the in-product
[cost explorer](../10-dashboard/monitoring.md#5-cost-explorer).

---

# 5. SLO Dashboard

Shows SLO attainment and error-budget burn for each service
(e.g. [Tool Runtime SLOs](../07-tool-runtime/observability-ops.md#6-service-level-objectives)),
feeding the [alerting](alerting.md) strategy.

---

# 6. Provisioning as Code

Dashboards are version-controlled (JSON/Grafana provisioning) and deployed with the
platform ([deployment](../12-deployment/kubernetes.md#10-observability)), so they are
reproducible and reviewed like any other artifact.

---

# 7. Native vs. Grafana

| Surface | Use |
|---------|-----|
| Native [dashboard](../10-dashboard/monitoring.md) | In-product, RBAC-scoped, tenant views |
| Grafana | Infra-level, cross-cutting operator views |

The native dashboard can embed Grafana panels for infrastructure metrics.

---

# 8. Access & Scoping

Native dashboards are tenant/project scoped by RBAC
([monitoring §9](../10-dashboard/monitoring.md#9-scoping--access)); operator Grafana
is access-controlled separately for platform staff.

---

# 9. Dependencies

- [`14-observability/metrics.md`](metrics.md)
- [`14-observability/tracing.md`](tracing.md)
- [`10-dashboard/monitoring.md`](../10-dashboard/monitoring.md)

---

# 10. Related Documents

- [`14-observability/alerting.md`](alerting.md)
- [`14-observability/index.md`](index.md)

---

# 11. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.1.0 | 2026-07-09 | Added a status note pointing to the real starter Grafana dashboard at `deployment/observability/dashboard.json` (RM-GA-P4 OBS-803) — one dashboard covering RED + LLM cost/tokens, not this doc's full 7-dashboard catalog |
| 1.0.0 | 2026-06-27 | Initial Dashboards specification |
