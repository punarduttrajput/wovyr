<!--
File: docs/17-adr/ADR-0008-subworkflows.md
Document ID: ADR-0008
-->

# ADR-0008: Child Workflows as Activities (not Inline Expansion)

**Status:** Accepted
**Date:** 2026-06-29
**Deciders:** Workflow Engine Team
**Supersedes:** —

---

# Context

The workflow engine compiles a [`Definition`](../../crates/apex-workflow/src/definition.rs)
to a **static, acyclic DAG** and executes it as a durable, event-sourced state
machine. There is no way to compose one workflow out of another: a workflow cannot
invoke another workflow as a unit, run it independently, and consume its result.
This is [gap-closure item G5](../03-workflow-engine/temporal-gap-analysis.md#g5--child--sub-workflows-investigate-first)
in the Temporal gap analysis, and it is the only remaining gap that touches the
static-DAG model — so it needs a recorded decision before implementation.

Two designs were identified in the gap analysis:

- **(a) Activity-as-subworkflow** — a new `workflow` activity type whose execution
  *starts a child execution* and suspends until it finishes, then exposes the
  child's result as the activity's output.
- **(b) Inline expansion** — compile a referenced sub-DAG into the parent DAG at
  validation time, so there is no runtime child at all.

Constraints that shaped the choice:

- The engine is **deterministic** and free of ambient clocks/randomness; ids are
  derived, never random ([coding-standards §7](../19-implementation-guide/coding-standards.md)).
- Durability, queries (G3), and visibility (G4) are all keyed on an **execution id**
  and the existing event-log + checkpoint stores.
- The engine already has a durable **suspend/resume** primitive (the `Interrupted`
  waiting state) and engine-native handling of `wait` activities.

---

# Decision

Implement **option (a): child workflows are activities.**

A `workflow`-typed activity names a child workflow (`name: <workflow-name>`). The
engine handles it natively (mirroring how it handles `wait`):

1. Resolve the child [`Definition`] by name via a `DefinitionResolver` attached to
   the engine (`Engine::with_subworkflows`).
2. Start the child as a **real execution** with a **derived id**
   `"<parent-id>::<activity-id>"` — deterministic, durable, and independently
   queryable/visible through the G3/G4 surfaces.
3. Drive the child with the engine's own `run`/`resume`. On:
   - **Completed** → the parent activity completes; its output is the child's final
     variables (so downstream activities and guards can aggregate child results).
   - **Failed / Compensated** → the parent activity fails terminally, which triggers
     the parent's normal saga compensation.
   - **Interrupted** (the child suspended, e.g. on its own `wait`) → the parent
     activity suspends too; resuming the parent re-drives the child.

The recursion (`drive → run_activity → run_subworkflow → run/resume → drive`) is
broken with a boxed future at the child-drive edge. `workflow` activities, like
`wait`, take the **sequential** scheduling path (excluded from the concurrent
ready-batch).

This is delivered as a **prototype** (engine + tests): a parent that fans out to
two children and aggregates their results, plus child-failure propagation.

---

# Consequences

**Positive**
- **Maximum reuse.** Children ride the existing durability, retry, compensation,
  suspend/resume, query (G3), and visibility (G4) machinery for free — a child is
  just another execution in the same store.
- **Each DAG stays static and acyclic**, preserving the validation guarantees and
  the model's core invariants. Composition happens *between* executions, not by
  mutating a DAG.
- **Independent lifecycle.** A child has its own id, checkpoint, event timeline, and
  status — debuggable and observable on its own.
- **Determinism preserved** — the child id is derived from parent id + activity id;
  no clocks or randomness enter.

**Negative / limitations (prototype scope)**
- Fan-out children currently run **sequentially** (the `workflow` activity uses the
  sequential path). Concurrent child execution is future work.
- **Input templating** (`${…}` from parent variables into child input) is a
  CLI-executor concern, not engine-native; the prototype passes the activity's
  static `inputs` as the child input. Threading parent data through is a follow-up.
- **Compensation does not cascade** into a completed child automatically; a parent
  can declare a `compensate` handler that launches a compensating child, but the
  engine does not auto-roll-back nested children. Future work.
- No depth/cycle guard yet on `workflow → workflow` chains; a recursive workflow
  reference could recurse unbounded. A static depth/cycle check is a follow-up.
- No CLI surface in the prototype (engine + tests only); wiring a multi-definition
  resolver into `apex-cli` is a follow-up.

---

# Alternatives Considered

- **(b) Inline expansion** — compiling the sub-DAG into the parent at validation
  time. Simpler runtime (no child execution), but it **loses independent lifecycle,
  retry, and visibility**, bloats the parent's event history, complicates id/namespace
  collisions, and makes a child's failure indistinguishable from the parent's. It
  also cannot represent a child that suspends on its own `wait`. Rejected: it trades
  away exactly the durability/observability properties that make the engine valuable.
- **Executor-driven sub-workflows** — handling the `workflow` type inside the
  `ActivityExecutor` (which would hold an `Engine` handle). Rejected: it creates an
  `Engine ↔ executor` reference cycle (Arc leak) and awkward chicken-and-egg
  construction; engine-native handling (like `wait`) avoids both.

---

# Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-29 | Initial decision: child workflows as activities (G5 prototype) |
