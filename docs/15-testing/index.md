<!--
File: docs/15-testing/index.md
Document ID: TEST-INDEX-001
-->

# Testing Index

**Document ID:** TEST-INDEX-001  
**File Path:** `docs/15-testing/index.md`  
**Version:** 1.0.0  
**Status:** Active  
**Owner:** Quality Engineering Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document is the **central navigation and strategy index** for testing the Apex AI Platform — the layers of testing, what each guarantees, and how they combine into a confident release pipeline.

---

# 2. Test Pyramid

```text
        ╱ chaos · security ╲          (resilience & safety)
       ╱  performance        ╲        (scale & latency)
      ╱   workflow / e2e       ╲      (behavior across services)
     ╱    integration            ╲    (service + datastore)
    ╱_____ unit (fast, many) ______╲  (logic in isolation)
```

Most coverage is fast unit tests; fewer, higher-value tests sit above. Determinism
is a platform requirement, which makes much of this testable
([workflow](../03-workflow-engine/workflow-dsl.md#13-loops),
[memory retrieval](../06-memory-engine/retrieval.md#11-determinism)).

---

# 3. Document Map

| Document | Responsibility |
|----------|----------------|
| [unit-tests.md](unit-tests.md) | Isolated logic, fast feedback |
| [integration-tests.md](integration-tests.md) | Services + real datastores |
| [workflow-tests.md](workflow-tests.md) | Workflow & agent behavior end to end |
| [performance-tests.md](performance-tests.md) | Latency, throughput, scale |
| [chaos-testing.md](chaos-testing.md) | Failure injection & resilience |
| [security-testing.md](security-testing.md) | Auth, isolation, supply chain |

---

# 4. Principles

1. **Deterministic first** — exploit the platform's determinism for reliable tests.
2. **Fast feedback** — unit tests run in seconds; gate every PR.
3. **Real dependencies where it matters** — integration uses real Postgres/Qdrant/NATS.
4. **Test the contracts** — API/DSL/plugin contracts have their own tests.
5. **Safety is tested** — isolation and authorization have explicit tests.
6. **CI-enforced** — coverage and quality gates in the pipeline.

---

# 5. CI Pipeline (Overview)

```text
PR → lint + unit → integration → contract → (nightly: perf · chaos · security)
                                   │
                                   ▼
                              merge gate
```

CI uses the [CLI](../11-cli/examples.md#8-cicd-pipeline-non-interactive) and runs
against ephemeral [Compose](../12-deployment/docker-compose.md) stacks.

---

# 6. Coverage Targets

Per-component targets (see [success metrics](../00-executive/success-metrics.md));
critical paths (auth, isolation, workflow execution) aim highest.

---

# 7. Dependencies

- [`19-implementation-guide`](../SUMMARY.md) *(planned: build system, standards)*
- [`12-deployment/docker-compose.md`](../12-deployment/docker-compose.md)
- [`14-observability/index.md`](../14-observability/index.md)

---

# 8. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Testing Index |
