<!--
File: docs/15-testing/workflow-tests.md
Document ID: TEST-003
-->

# Workflow & Agent Testing

**Document ID:** TEST-003  
**File Path:** `docs/15-testing/workflow-tests.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Quality Engineering Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines **end-to-end behavioral testing** of workflows and agents — verifying that authored definitions produce correct outcomes across the full runtime, including durability, retries, compensation, and agent reasoning.

---

# 2. Workflow Testing

## 2.1 Levels

| Level | Verifies |
|-------|----------|
| Definition | DSL validates and compiles ([validation rules](../03-workflow-engine/workflow-dsl.md#24-validation-rules)) |
| Execution | Correct path, outputs, and state transitions |
| Durability | Survives restart via [checkpointing](../03-workflow-engine/checkpointing-specification.md) |
| Resilience | [Retry](../03-workflow-engine/retry-engine.md) and [compensation](../03-workflow-engine/compensation-engine.md) behave |

## 2.2 Deterministic Replay

Because execution is deterministic, tests **replay** an execution from its event
history and assert identical state — catching non-determinism and validating
checkpoint/resume.

## 2.3 Time & Events

Tests use a fake clock to fast-forward [timers](../03-workflow-engine/workflow-dsl.md#18-timer)
and inject [events/signals](../09-api/workflows.md#7-signals--events) to drive
waits, rather than waiting in real time.

---

# 3. Agent Testing

## 3.1 Behavioral Tests

Run an agent against fixed inputs with a **fake provider** returning scripted
model responses, asserting the agent plans, calls the right
[tools](../07-tool-runtime/execution-api.md), and produces expected output.

## 3.2 Trace Assertions

Tests assert on the [run trace](../09-api/agents.md#6-run-lifecycle--streaming):
planner steps, tool calls and arguments, and memory reads — verifying *how* the
agent reached its answer, not just the final text.

## 3.3 Tool & Memory Stubs

- Tools can run as deterministic stubs or in the real
  [Tool Runtime](../07-tool-runtime/index.md) sandbox.
- Memory is seeded with fixtures so [retrieval](../06-memory-engine/retrieval.md) is
  reproducible.

---

# 4. Evaluation (Quality) Testing

Beyond pass/fail, agents are evaluated for quality:

- Test cases with **golden outputs** or rubric-based grading.
- Version-over-version comparison (quality, cost, latency) in
  [Agent Studio](../10-dashboard/agent-studio.md#6-evaluation-planned-integration).
- Regression gates: a new agent/prompt version must not regress key metrics.

LLM-as-judge grading uses a pinned judge model via the
[LLM Gateway](../05-llm-gateway/index.md) for repeatability.

---

# 5. Golden / Snapshot Suites

Curated suites of representative workflows and agent scenarios run in CI as
snapshots; intentional output changes update the snapshot under review.

---

# 6. Human Tasks & Approvals

Tests drive [human tasks](../09-api/workflows.md#8-human-tasks) programmatically
(approve/reject) to cover suspended/resumed paths and
[separation-of-duties](../13-security/rbac.md#9-separation-of-duties) rules.

---

# 7. CI Integration

A core set runs per PR; broader evaluation suites run nightly (they can be slower
and cost provider tokens where live models are used).

---

# 8. Dependencies

- [`03-workflow-engine/execution-model.md`](../03-workflow-engine/execution-model.md)
- [`04-agent-framework/agent-runtime-protocol.md`](../04-agent-framework/agent-runtime-protocol.md)
- [`15-testing/integration-tests.md`](integration-tests.md)

---

# 9. Related Documents

- [`15-testing/index.md`](index.md)
- [`15-testing/performance-tests.md`](performance-tests.md)

---

# 10. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Workflow & Agent Testing specification |
