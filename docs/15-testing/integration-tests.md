<!--
File: docs/15-testing/integration-tests.md
Document ID: TEST-002
-->

# Integration Testing

**Document ID:** TEST-002  
**File Path:** `docs/15-testing/integration-tests.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Quality Engineering Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines **integration testing** — verifying that services work correctly against **real datastores and neighboring services**, catching issues that unit tests (with mocks) cannot.

---

# 2. Scope

| Boundary | Examples |
|----------|----------|
| Service ↔ datastore | Memory Engine ↔ Postgres + Qdrant |
| Service ↔ service | Agent Runtime ↔ LLM Gateway ↔ Tool Runtime |
| API contracts | Endpoint behavior, errors, pagination, idempotency |
| Event flows | `plugin.*`, `execution.*` over NATS |

---

# 3. Environment

Integration tests run against **ephemeral real backends**, started via
[Docker Compose](../12-deployment/docker-compose.md) (Postgres, Redis, Qdrant,
NATS, MinIO) or Testcontainers:

```text
spin up compose (core profile) → migrate → seed → run tests → tear down
```

This matches the team self-host topology, so tests exercise the real wiring.

---

# 4. Provider Handling

- LLM providers are replaced by a **recorded/fake provider** behind the
  [LLM Gateway](../05-llm-gateway/index.md) so tests are deterministic and free of
  external cost.
- A small suite of **live provider** smoke tests runs separately (gated, optional)
  to catch real-provider drift.

---

# 5. Contract Tests

Each external contract has tests asserting the documented behavior:

- [Platform API](../09-api/overview.md) — status codes, error envelope, pagination,
  idempotency, concurrency (`ETag`/`If-Match`).
- [Workflow DSL](../03-workflow-engine/workflow-dsl.md) — validation rules, WIR
  compilation.
- [Tool Runtime Execution API](../07-tool-runtime/execution-api.md) and
  [Memory Engine API](../06-memory-engine/memory-api.md).
- [Plugin manifest](../08-plugin-sdk/plugin-api.md) — install/verify/register.

Contract tests guard against accidental breaking changes.

---

# 6. Data & Isolation

- Each test run uses an isolated tenant/namespace; tests assert **no cross-tenant
  leakage** (a hard requirement — [authorization](../13-security/authorization.md#5-tenant-isolation)).
- Fixtures are created and torn down per test/suite to avoid coupling.

---

# 7. Event-Driven Assertions

For async flows, tests subscribe to the
[Event Bus](../02-architecture/event-driven-architecture.md) and assert the
expected `*.completed`/`*.failed` events occur, rather than sleeping.

---

# 8. Migration Tests

Database [migrations](../12-deployment/docker-compose.md#5-initialization) are
tested forward (and where supported, backward) to ensure safe upgrades.

---

# 9. CI Integration

Integration tests run on PRs (after unit) against a fresh stack and must pass to
merge ([CI pipeline](index.md#5-ci-pipeline-overview)).

---

# 10. Dependencies

- [`12-deployment/docker-compose.md`](../12-deployment/docker-compose.md)
- [`15-testing/workflow-tests.md`](workflow-tests.md)
- [`02-architecture/event-driven-architecture.md`](../02-architecture/event-driven-architecture.md)

---

# 11. Related Documents

- [`15-testing/index.md`](index.md)
- [`15-testing/unit-tests.md`](unit-tests.md)

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Integration Testing specification |
