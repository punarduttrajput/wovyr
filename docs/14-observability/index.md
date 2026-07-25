<!--
File: docs/14-observability/index.md
Document ID: OBS-INDEX-001
-->

# Observability Index

**Document ID:** OBS-INDEX-001  
**File Path:** `docs/14-observability/index.md`  
**Version:** 1.0.0  
**Status:** Active  
**Owner:** Platform Operations Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document is the **central navigation and architecture index** for observability across the Wovyr AI Platform — logging, metrics, tracing, dashboards, and alerting. **Observable by Default** is a [core principle](../00-executive/vision.md).

This section is the platform-wide reference; subsystem docs (e.g.
[Tool Runtime Observability](../07-tool-runtime/observability-ops.md)) implement it
for their domain.

---

# 2. The Three Pillars (+ Events)

```text
Logs        ── what happened (structured, correlated)
Metrics     ── how much / how fast (Prometheus)
Traces      ── where time went (OpenTelemetry, end-to-end)
Events      ── domain signals (Event Bus) for cost/usage/alerts
```

All four share correlation IDs (`request_id`, `trace_id`) so a symptom in one
pillar links to the others.

---

# 3. Stack

| Concern | Technology |
|---------|-----------|
| Instrumentation | OpenTelemetry (traces, metrics, logs) |
| Metrics store | Prometheus |
| Dashboards | Grafana + native dashboard |
| Tracing backend | OTLP-compatible (e.g. Tempo/Jaeger) |
| Log aggregation | Structured log pipeline (e.g. Loki/ELK) |

Per [tech mapping](../02-architecture/c4-container.md#11-technology-mapping) and
[deployment](../12-deployment/index.md).

---

# 4. Document Map

| Document | Responsibility |
|----------|----------------|
| [logging.md](logging.md) | Structured logging standards |
| [metrics.md](metrics.md) | Metric taxonomy (RED/USE), cost metrics |
| [tracing.md](tracing.md) | Distributed tracing and correlation |
| [dashboards.md](dashboards.md) | Standard dashboards and golden signals |
| [alerting.md](alerting.md) | Alert rules, SLOs, on-call |

---

# 5. Principles

1. **Instrument everything** — every service emits all three pillars.
2. **Correlate** — one ID threads logs ↔ traces ↔ metrics.
3. **Golden signals** — latency, traffic, errors, saturation everywhere.
4. **Cost is a signal** — token/tool/memory spend is first-class.
5. **Observability ≠ audit** — see [audit](../13-security/audit.md) for security-grade records.

---

# 6. Relationship to Audit

Observability is for **operations** (may be sampled, shorter retention). Security
**audit** ([13-security/audit.md](../13-security/audit.md)) is integrity-protected
and compliance-retained. They are separate pipelines.

---

# 7. Dependencies

- [`02-architecture/c4-container.md`](../02-architecture/c4-container.md)
- [`07-tool-runtime/observability-ops.md`](../07-tool-runtime/observability-ops.md)
- [`00-executive/success-metrics.md`](../00-executive/success-metrics.md)

---

# 8. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Observability Index |
