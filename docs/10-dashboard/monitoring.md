<!--
File: docs/10-dashboard/monitoring.md
Document ID: DASH-006
-->

# Monitoring & Cost Dashboards

**Document ID:** DASH-006  
**File Path:** `docs/10-dashboard/monitoring.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document specifies the **monitoring and cost** surfaces of the dashboard — the operational views that show platform health, live activity, and spend across agents, workflows, tools, memory, and model usage.

It is the human-facing layer over the platform's telemetry and the planned
[Observability](../SUMMARY.md) section.

---

# 2. Surfaces

| View | Shows |
|------|-------|
| Home / Health | Service health, error budgets, throughput |
| Activity | Live agent runs and workflow executions |
| Cost Explorer | Spend by tenant/project/agent/model over time |
| Usage | Tokens, tool executions, memory operations |
| Alerts | Active alerts and recent incidents |

---

# 3. Health Overview

Aggregates `/healthz`/`/readyz`/`/metrics` and SLO status across services
(API Gateway, Agent Runtime, Workflow Engine, LLM Gateway, Memory Engine,
Tool Runtime, Plugin Engine):

- Up/degraded/down per service
- SLO attainment and error-budget burn (e.g.
  [Tool Runtime SLOs](../07-tool-runtime/observability-ops.md#6-service-level-objectives))
- Request rate, error rate, latency (RED metrics)

---

# 4. Live Activity

Real-time feed of in-flight work, streamed via the
[BFF websocket bridge](overview.md#7-real-time):

- Agent runs ([status](../09-api/agents.md#6-run-lifecycle--streaming))
- Workflow executions ([status](../09-api/workflows.md#6-execution-lifecycle))
- Tool executions ([status](../07-tool-runtime/observability-ops.md#3-key-metrics))

Drill into any item to its trace/step inspector.

---

# 5. Cost Explorer

A first-class spend view fed by platform **cost events**:

```text
Cost events (LLM Gateway · Tool Runtime · Memory)
        │  Event Bus → analytics
        ▼
   Cost Explorer (by tenant / project / agent / model / time)
```

- Model spend from [LLM Gateway token management](../05-llm-gateway/token-management.md#9-cost-events)
- Tool execution cost from [Tool Runtime](../07-tool-runtime/observability-ops.md#8-capacity--cost)
- Cache savings from [LLM Gateway](../05-llm-gateway/caching.md) and
  [Memory compression](../06-memory-engine/compression.md)
- Budget/quota utilization vs. [project quotas](../09-api/projects.md#5-quotas)

Supports grouping, filtering, and CSV export.

---

# 6. Usage Analytics

- Token usage by type (prompt/completion/cached)
- Tool execution counts, durations, success/error rates
- Memory operations, recall proxies, index growth
- Trends and anomalies

---

# 7. Alerts

Surfaces active alerts (e.g. queue backlog, provider failover spikes, egress
anomalies, budget breaches) with links to the relevant
[runbooks](../07-tool-runtime/observability-ops.md#9-runbooks). Alert routing and
rules belong to the platform [Observability](../SUMMARY.md) section; this view
consumes them.

---

# 8. Drill-Down & Correlation

Every metric/alert links to underlying traces using the shared `request_id`/trace
context ([Overview §14](overview.md#14-observability)), so an operator can go from a
cost spike or error rate to the exact runs that caused it.

---

# 9. Scoping & Access

- Views are tenant/project scoped; users see only their authorized scope.
- Cross-project/organization roll-ups require `org.admin`/`platform.admin`.
- All access is audited.

---

# 10. Embedding External Dashboards

Where deployments run Grafana/Prometheus
([deployment architecture](../02-architecture/deployment-architecture.md)), the
dashboard can embed those panels alongside native views for infrastructure-level
metrics.

---

# 11. Dependencies

- [`05-llm-gateway/token-management.md`](../05-llm-gateway/token-management.md)
- [`07-tool-runtime/observability-ops.md`](../07-tool-runtime/observability-ops.md)
- [`09-api/projects.md`](../09-api/projects.md)
- [`00-executive/success-metrics.md`](../00-executive/success-metrics.md)

---

# 12. Related Documents

- [`10-dashboard/overview.md`](overview.md)
- [`10-dashboard/settings.md`](settings.md)

---

# 13. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Monitoring & Cost Dashboards specification |
