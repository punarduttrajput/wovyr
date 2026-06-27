<!--
File: docs/07-tool-runtime/observability-ops.md
Document ID: TRT-006
-->

# Tool Runtime Observability & Operations

**Document ID:** TRT-006  
**File Path:** `docs/07-tool-runtime/observability-ops.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines how the Tool Runtime is **observed and operated**: the logs, metrics, traces, and audit it emits; its health surfaces; its service-level objectives; and the runbooks for common operational situations.

It complements the framework's
[audit](../04-agent-framework/tool-framework.md#53-audit-logging),
[metrics](../04-agent-framework/tool-framework.md#54-metrics-collection),
[tracing](../04-agent-framework/tool-framework.md#55-tracing), and
[health](../04-agent-framework/tool-framework.md#56-health-monitoring) sections
with the operational view of the running service.

---

# 2. Telemetry Signals

| Signal | Examples |
|--------|----------|
| Logs | Structured, correlation-ID tagged, per execution stage |
| Metrics | Queue time, start latency, run duration, success/error rate, resource use, warm-pool hit ratio |
| Traces | OpenTelemetry spans: dispatch → authorize → schedule → sandbox → execute |
| Audit | Per-execution tamper-evident record (see [Security §11](security-isolation.md#11-audit)) |
| Events | `tool.execution.*` published to the [Event Bus](../03-workflow-engine/event-bus.md) |

All signals carry `tenant`, `tool`, `version`, and `execution_id` for correlation.

---

# 3. Key Metrics

```text
# Throughput / latency
tool_executions_total{tool,version,status}
tool_queue_seconds{pool}            (histogram)
tool_start_latency_seconds{backend} (histogram)
tool_duration_seconds{tool}         (histogram)

# Resources
tool_cpu_ms{tool}
tool_peak_memory_bytes{tool}
tool_egress_bytes{tenant}

# Fleet
worker_utilization{pool}
warm_pool_hit_ratio{backend}
sandbox_provision_failures_total{backend}

# Safety
authorization_denied_total{reason}
resource_exceeded_total{resource}
sandbox_killed_total{cause}
```

---

# 4. Tracing

A single execution produces one trace spanning the control and data planes:

```text
span: execution
 ├─ dispatch (resolve tool/version)
 ├─ authorize (policy)
 ├─ schedule (worker selection + queue wait)
 ├─ sandbox.provision (backend, warm/cold)
 ├─ execute (tool run)
 └─ teardown (destroy + reclaim)
```

Traces propagate the caller's `correlation_id` so a tool execution links back to
the originating agent goal or workflow step.

---

# 5. Health Surfaces

| Endpoint | Meaning |
|----------|---------|
| `/healthz` | Process liveness |
| `/readyz` | Ready to accept work (registry reachable, ≥1 worker pool healthy) |
| `/metrics` | Prometheus metrics |

Workers additionally report capability, capacity, and load to the scheduler via
heartbeat (see [Worker Pool §10](worker-pool.md#10-health--lifecycle)).

---

# 6. Service-Level Objectives

| SLO | Target |
|-----|--------|
| Control-plane availability | 99.99% |
| Interactive start latency (warm) | p95 < 20 ms |
| Queue wait (healthy fleet) | p95 < 50 ms |
| Execution success rate (excl. tool_error) | > 99.9% |
| Cross-tenant isolation violations | 0 |

Error-budget burn on these SLOs drives alerting and scale decisions.

---

# 7. Alerting

| Alert | Condition |
|-------|-----------|
| High queue wait | `tool_queue_seconds` p95 > SLO for 5m |
| Provision failures | `sandbox_provision_failures_total` rate spike |
| Elevated kills | `sandbox_killed_total{cause=oom|timeout}` spike |
| Authorization spike | `authorization_denied_total` anomalous (possible misconfig/abuse) |
| Egress anomaly | per-tenant `tool_egress_bytes` exceeds baseline |
| Warm-pool starvation | `warm_pool_hit_ratio` < threshold |

---

# 8. Capacity & Cost

- Per-tenant execution counts, durations, and resource usage feed showback/chargeback.
- Cost events publish to the [Event Bus](../03-workflow-engine/event-bus.md) and roll
  up in the dashboard alongside [LLM Gateway cost](../05-llm-gateway/token-management.md)
  and [Memory Engine](../06-memory-engine/overview.md) usage.
- Warm-pool sizing is tuned to balance start-latency SLOs against idle cost.

---

# 9. Runbooks

| Situation | Action |
|-----------|--------|
| Queue backing up | Verify autoscaler; check for a hot tool/tenant; raise pool min |
| A tool failing widely | Inspect traces/audit; pin/rollback tool version in registry |
| Sandbox escape suspicion | Quarantine pool; force microVM floor; rotate node images |
| Worker node unhealthy | Drain and recycle; reschedule idempotent in-flight work |
| Secret leak suspected | Rotate affected secret refs; audit `secrets_used`; revoke grants |
| Cost spike | Identify tenant/tool via metrics; apply rate limit/quota |

---

# 10. Operational Controls

- **Tool version pinning / rollback** via the registry without redeploying the Runtime.
- **Quarantine** a tool or worker pool to stop execution immediately.
- **Drain** workers for maintenance with no dropped in-flight work.
- **Rate-limit / quota** adjustments per tenant/tool applied live.

---

# 11. Dependencies

- [`03-workflow-engine/event-bus.md`](../03-workflow-engine/event-bus.md)
- [`04-agent-framework/tool-framework.md`](../04-agent-framework/tool-framework.md#53-audit-logging)
- [`14-observability`](../SUMMARY.md) *(planned: platform observability)*

---

# 12. Related Documents

- [`07-tool-runtime/overview.md`](overview.md)
- [`07-tool-runtime/worker-pool.md`](worker-pool.md)
- [`07-tool-runtime/security-isolation.md`](security-isolation.md)

---

# 13. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Tool Runtime Observability & Operations specification |
