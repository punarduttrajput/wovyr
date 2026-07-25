<!--
File: docs/14-observability/alerting.md
Document ID: OBS-005
-->

# Observability: Alerting & SLOs

**Document ID:** OBS-005  
**File Path:** `docs/14-observability/alerting.md`  
**Version:** 1.1.0  
**Status:** Draft (aspirational multi-service catalog) — a **real, working starter**
now exists at [`deployment/observability/alerts.yml`](../../deployment/observability/alerts.yml)
(RM-GA-P4 OBS-803): 7 Prometheus rules over the actual metrics the single-node
server emits (`promtool`-validated), not the full SLO/routing/runbook program this
document describes. See that directory's `README.md` for what it does and doesn't
cover.  
**Owner:** Platform Operations Team  
**Last Updated:** 2026-07-09

---

# 1. Purpose

This document defines the **alerting** strategy for the Wovyr AI Platform — SLOs, alert rules, routing, and on-call response — so problems are caught and escalated before users feel them.

---

# 2. SLO-Driven Alerting

Alerts are driven primarily by **SLOs and error-budget burn**, not raw thresholds,
to reduce noise:

```text
SLI (from metrics) → SLO target → error budget → burn-rate alert
```

Fast burn (budget exhausting quickly) pages; slow burn raises a ticket. Each
service publishes SLOs (e.g.
[Tool Runtime](../07-tool-runtime/observability-ops.md#6-service-level-objectives)).

---

# 3. Core SLOs

| SLO | Example target |
|-----|----------------|
| API availability | 99.99% |
| API latency | p95 < 200 ms |
| Agent run success | > 99.5% |
| Tool start latency (warm) | p95 < 20 ms |
| Memory retrieval | p95 < 30 ms |
| Cross-tenant isolation violations | 0 |

---

# 4. Alert Catalog

| Alert | Condition | Severity |
|-------|-----------|----------|
| Service down | `/readyz` failing across replicas | page |
| Error-budget fast burn | SLO burn > 14x for 5m | page |
| LLM provider degraded | failover spike / all providers failing | page |
| Tool queue backlog | `tool_queue_seconds` p95 > SLO | warn→page |
| Budget breach | tenant cost > quota | warn |
| Egress anomaly | per-tenant egress > baseline | warn |
| Authz denial spike | `authorization_denied_total` anomalous | warn (security) |
| Cache degraded | hit ratio < threshold | warn |

These derive from [metrics](metrics.md) and security
[audit](../13-security/audit.md#9-alerting-on-audit) signals.

---

# 5. Routing & Escalation

```text
Alert fires
   │
   ▼
Severity → channel (page / chat / ticket)
   │
   ▼
On-call ack → investigate (dashboards → traces → logs)
   │
   ▼
Escalate if unacked / unresolved within policy window
```

Routing integrates with paging tools (PagerDuty/Opsgenie) and chat; severity
determines channel.

---

# 6. Runbooks

Every page links to a **runbook**. Subsystem runbooks already exist (e.g.
[Tool Runtime runbooks](../07-tool-runtime/observability-ops.md#9-runbooks)); this
section is the catalog and on-call entry point.

---

# 7. Noise Control

- Alert on **symptoms** (user-facing SLOs), not every cause.
- Group/inhibit related alerts (one root cause → one page).
- Tune with burn-rate windows; review alert quality regularly (alert on alerts that
  never action).

---

# 8. Security Alerts

Security-relevant alerts (authz denial spikes, secret-access anomalies, mass
exports) route to the security on-call and reference
[audit](../13-security/audit.md), kept distinct from operational paging.

---

# 9. Synthetic & Health Checks

Synthetic probes exercise critical user journeys (login, agent run, workflow run)
to catch outages independent of traffic, complementing real-traffic SLIs.

---

# 10. Dependencies

- [`14-observability/metrics.md`](metrics.md)
- [`14-observability/dashboards.md`](dashboards.md)
- [`13-security/audit.md`](../13-security/audit.md)

---

# 11. Related Documents

- [`07-tool-runtime/observability-ops.md`](../07-tool-runtime/observability-ops.md)
- [`14-observability/index.md`](index.md)

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.1.0 | 2026-07-09 | Added a status note pointing to the real, `promtool`-validated starter rule set at `deployment/observability/alerts.yml` (RM-GA-P4 OBS-803) — a small, real subset of this doc's full aspirational alert catalog |
| 1.0.0 | 2026-06-27 | Initial Alerting & SLOs specification |
