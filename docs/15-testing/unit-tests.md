<!--
File: docs/15-testing/unit-tests.md
Document ID: TEST-001
-->

# Unit Testing

**Document ID:** TEST-001  
**File Path:** `docs/15-testing/unit-tests.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Quality Engineering Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines **unit testing** practice for the Apex AI Platform — fast, isolated tests of individual functions and modules that form the base of the [test pyramid](index.md#2-test-pyramid).

---

# 2. Scope

Unit tests cover pure logic without external dependencies:

- DSL parsing/validation, expression evaluation
- Routing/selection logic ([LLM Gateway routing](../05-llm-gateway/routing.md))
- Ranking/fusion math ([Memory ranking](../06-memory-engine/ranking.md))
- State-machine transitions ([Workflow state machine](../03-workflow-engine/state-machine.md))
- Serialization, schema validation, token accounting

External systems (DB, providers, network) are **mocked or faked**.

---

# 3. Tooling

- Rust: built-in `#[test]`, `cargo test`, `cargo nextest` for speed.
- Property-based testing (`proptest`) for parsers, expressions, and invariants.
- Snapshot testing for stable serialized outputs.
- Frontend: component/unit tests for the [Angular dashboard](../10-dashboard/overview.md).

---

# 4. What Makes a Good Unit Test

- Tests one behavior; named for the behavior.
- Deterministic — no clock/network/random unless injected.
- Fast (sub-ms to ms); the whole suite runs in seconds.
- Arrange–Act–Assert structure; minimal mocking.

---

# 5. Determinism Helpers

Because the platform mandates determinism, units inject time, randomness, and IDs
so tests are reproducible:

```rust
let clock = FakeClock::at("2026-06-27T10:00:00Z");
let ids   = SeqIdGen::new("test");
let engine = Engine::new(clock, ids);
```

This mirrors the runtime's deterministic execution requirements
([workflow loops](../03-workflow-engine/workflow-dsl.md#13-loops)).

---

# 6. Mocks & Fakes

| Dependency | Test double |
|------------|-------------|
| LLM provider | Fake provider returning canned responses |
| Vector store | In-memory fake |
| Event bus | In-memory channel |
| Secret vault | Fake resolver |

Prefer **fakes** (working in-memory implementations) over brittle mocks where
feasible.

---

# 7. Property & Fuzz Testing

- Property tests assert invariants (e.g. "validated DSL always compiles to a
  reachable graph").
- Fuzzing targets parsers and schema validators for robustness against malformed
  input (ties to [security testing](security-testing.md)).

---

# 8. Coverage & Gating

- Unit tests run on every PR and must pass to merge.
- Coverage is measured; critical modules (auth, isolation, DSL) target the highest
  bar ([success metrics](../00-executive/success-metrics.md)).

---

# 9. Dependencies

- [`15-testing/integration-tests.md`](integration-tests.md)
- [`19-implementation-guide`](../SUMMARY.md) *(planned: build/test commands)*

---

# 10. Related Documents

- [`15-testing/index.md`](index.md)
- [`15-testing/workflow-tests.md`](workflow-tests.md)

---

# 11. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Unit Testing specification |
