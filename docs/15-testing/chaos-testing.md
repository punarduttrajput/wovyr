<!--
File: docs/15-testing/chaos-testing.md
Document ID: TEST-005
-->

# Chaos Testing

**Document ID:** TEST-005  
**File Path:** `docs/15-testing/chaos-testing.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Quality Engineering Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines **chaos testing** — deliberately injecting failures to verify the Apex AI Platform degrades gracefully and recovers, validating the resilience mechanisms specified across subsystems.

---

# 2. Hypothesis-Driven Approach

```text
State a steady-state hypothesis (e.g. "agent runs succeed > 99.5%")
   │
   ▼
Inject a fault (kill a provider, a worker, a DB replica)
   │
   ▼
Observe: does steady state hold? does it recover?
   │
   ▼
Fix gaps; automate the experiment
```

Experiments start small (one fault, staging) and graduate to scheduled production
game-days.

---

# 3. Fault Catalog

| Fault | Validates |
|-------|-----------|
| LLM provider down/slow | [Gateway failover & circuit breaking](../05-llm-gateway/resilience.md) |
| Tool worker killed | [Reschedule of idempotent work](../07-tool-runtime/worker-pool.md#10-health--lifecycle) |
| Sandbox provision failure | [Retry on another worker](../07-tool-runtime/sandbox-runtime.md#9-failure--recovery) |
| Qdrant unavailable | [Degraded keyword retrieval](../06-memory-engine/retrieval.md#10-degraded-retrieval) |
| Redis unavailable | Hot-cache bypass, still durable |
| Postgres failover | Reads from replica; write recovery |
| NATS partition | Event delivery + at-least-once handling |
| Network latency/loss | Timeouts, retries, backpressure |
| Node loss | Rescheduling, drain behavior |

---

# 4. Resilience Assertions

Each experiment asserts the documented behavior, e.g.:

- A provider outage produces **successful failover** with no user-visible failure
  while a healthy provider exists ([resilience](../05-llm-gateway/resilience.md#5-failover)).
- A worker crash mid-execution **reschedules** idempotent tools and surfaces clear
  errors for non-idempotent ones.
- Cache/store outages **degrade** (not fail), flagged via `degraded` responses.

---

# 5. Blast-Radius Controls

- Experiments are scoped (one tenant/zone) with automatic **abort** if steady-state
  SLOs breach beyond a guardrail.
- Production game-days run in low-traffic windows with on-call present and a
  rollback plan.

---

# 6. Tooling

- Fault injection at the infra layer (e.g. Chaos Mesh/Litmus on Kubernetes).
- Application-level fault hooks (configurable error/latency injection) for targeted
  experiments behind a flag.

---

# 7. Observability During Chaos

Experiments rely on [metrics, traces, alerts](../14-observability/index.md) to
confirm detection and recovery — a fault that fires no alert is itself a finding.

---

# 8. Recovery Verification

After each fault, verify the system **returns to steady state** automatically
(self-healing) within target time, and that no data was lost (system of record
intact, derived stores rebuilt — [storage recovery](../06-memory-engine/storage-architecture.md#9-reindex--recovery)).

---

# 9. Cadence

- Automated chaos in staging on a schedule.
- Periodic production game-days for major releases.

---

# 10. Dependencies

- [`05-llm-gateway/resilience.md`](../05-llm-gateway/resilience.md)
- [`07-tool-runtime/worker-pool.md`](../07-tool-runtime/worker-pool.md)
- [`14-observability/alerting.md`](../14-observability/alerting.md)

---

# 11. Related Documents

- [`15-testing/index.md`](index.md)
- [`15-testing/performance-tests.md`](performance-tests.md)

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Chaos Testing specification |
