<!--
File: docs/15-testing/performance-tests.md
Document ID: TEST-004
-->

# Performance Testing

**Document ID:** TEST-004  
**File Path:** `docs/15-testing/performance-tests.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Quality Engineering Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines **performance testing** for the Wovyr AI Platform — validating that services meet their latency and throughput [non-functional requirements](../07-tool-runtime/overview.md#9-non-functional-requirements) under realistic and extreme load.

---

# 2. Test Types

| Type | Question |
|------|----------|
| Load | Does it meet SLOs at expected load? |
| Stress | Where does it break, and how? |
| Soak | Stable over hours/days (leaks, drift)? |
| Spike | Does it absorb sudden surges? |
| Capacity | How much per node; how does it scale? |

---

# 3. Targets (examples)

Performance tests assert documented NFRs, e.g.:

| Metric | Target |
|--------|--------|
| API p95 latency | < 200 ms |
| LLM Gateway overhead (non-cached) | < 8 ms p95 |
| Memory warm retrieval | < 30 ms p95 |
| Tool warm sandbox start | < 20 ms p95 |
| Throughput | per-service NFR targets |

Sources: [LLM Gateway](../05-llm-gateway/overview.md#9-non-functional-requirements),
[Memory](../06-memory-engine/overview.md#10-non-functional-requirements),
[Tool Runtime](../07-tool-runtime/overview.md#9-non-functional-requirements).

---

# 4. Methodology

```text
Define scenario + load profile
   │
   ▼
Run against a production-like environment (real backends, fake providers)
   │
   ▼
Measure p50/p95/p99 latency, throughput, error rate, saturation
   │
   ▼
Compare to targets + previous baseline (regression check)
```

Providers are faked with realistic latency so model variance does not skew results;
a separate live-provider profile measures real end-to-end latency.

---

# 5. Workloads

- Agent runs (mixed tool/model usage)
- Workflow executions (parallel branches, fan-out)
- Memory retrieval at scale (millions–billions of records)
- Tool execution fan-out across [worker pools](../07-tool-runtime/worker-pool.md)
- LLM Gateway routing/failover under concurrency

---

# 6. Scale Testing

Memory and tool subsystems are tested at platform-scale targets (e.g.
[billions of memories](../06-memory-engine/overview.md#10-non-functional-requirements),
thousands of concurrent executions) to validate sharding, indexing, and autoscaling.

---

# 7. Autoscaling Validation

Spike tests verify [autoscaling](../12-deployment/kubernetes.md#5-autoscaling)
reacts within target (e.g. add tool-worker capacity < 30 s) and that
[fair scheduling](../07-tool-runtime/worker-pool.md#5-fair-scheduling--concurrency)
holds under contention.

---

# 8. Tooling & Observability

- Load generators (k6/Gatling/custom Rust harness).
- Results correlate with [metrics/traces](../14-observability/index.md) captured
  during the run, using exemplars to find slow paths.

---

# 9. Regression Gating

Baselines are stored; nightly/release perf runs fail if p95/throughput regress
beyond a threshold versus baseline.

---

# 10. Dependencies

- [`14-observability/metrics.md`](../14-observability/metrics.md)
- [`07-tool-runtime/worker-pool.md`](../07-tool-runtime/worker-pool.md)
- [`12-deployment/kubernetes.md`](../12-deployment/kubernetes.md)

---

# 11. Related Documents

- [`15-testing/index.md`](index.md)
- [`15-testing/chaos-testing.md`](chaos-testing.md)

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Performance Testing specification |
