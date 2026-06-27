<!--
File: docs/10-dashboard/workflow-builder.md
Document ID: DASH-002
-->

# Workflow Builder (Visual Studio)

**Document ID:** DASH-002  
**File Path:** `docs/10-dashboard/workflow-builder.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document specifies the **Workflow Builder** — the visual studio for authoring, validating, and operating workflows without hand-writing YAML. It is a graphical front end over the [Workflow DSL](../03-workflow-engine/workflow-dsl.md) and the [Workflows API](../09-api/workflows.md).

---

# 2. Core Idea: Canvas ⇄ DSL

The builder maintains a **two-way mapping** between a visual graph and the DSL:

```text
Canvas (nodes + edges)  ⇄  Workflow DSL (YAML/JSON)  ⇄  WIR (compiled graph)
```

- Edits on the canvas update the DSL; editing the DSL updates the canvas.
- The canonical artifact is the DSL; the canvas is a faithful rendering of it.
- On save, the DSL is submitted to
  [`:validate`](../09-api/workflows.md#3-endpoints) → compiled to the
  [WIR](../03-workflow-engine/workflow-dsl.md#25-workflow-intermediate-representation-wir).

This guarantees the visual tool never produces a workflow the engine can't run.

---

# 3. Node Palette

Nodes correspond to DSL [activity types](../03-workflow-engine/workflow-dsl.md#9-activity-types):

| Node | DSL type |
|------|----------|
| Function | `function` |
| HTTP / gRPC | `http` / `grpc` |
| AI Activity | `ai` |
| Tool | `tool` |
| Script | `script` |
| Human Task | `human` |
| Event Wait | `event` |
| Timer | `timer` |
| Sub-workflow | `subprocess` |

Control-flow constructs (branch, parallel, loop) render as structural nodes mapping
to DSL [branches](../03-workflow-engine/workflow-dsl.md#11-conditional-branching),
[parallel](../03-workflow-engine/workflow-dsl.md#12-parallel-execution), and
[loops](../03-workflow-engine/workflow-dsl.md#13-loops).

---

# 4. Authoring Features

- Drag-and-drop nodes; connect with typed edges (transitions).
- Per-node config panels with schema-driven forms (tool/AI inputs validated against
  their schemas from the [Tools API](../09-api/tools.md#6-schema-introspection)).
- Expression editor for [conditions](../03-workflow-engine/workflow-dsl.md#11-conditional-branching)
  with autocomplete over workflow variables.
- Inline retry/compensation/timeout configuration per node.
- Variable and input definitions panel.

---

# 5. Live Validation

As the user edits, the builder surfaces the engine's
[validation rules](../03-workflow-engine/workflow-dsl.md#24-validation-rules):

- Unreachable nodes, orphaned transitions, duplicate IDs
- Invalid expressions
- Missing compensation mappings
- Unresolved tool/permission references

Errors are shown on the offending node and in a problems panel before save.

---

# 6. Versioning & Diff

- Saving creates a draft; **publish** produces an immutable
  [workflow_version](../09-api/workflows.md#10-versioning).
- A visual **diff** compares two versions (added/removed/changed nodes and edges).
- Running executions continue on their start version.

---

# 7. Run & Observe

From the builder a user can:

- `:run` a workflow with an input form derived from its declared inputs.
- Watch the execution **animate on the canvas** in real time — nodes highlight as
  they enter `running`, `completed`, `failed`, or `compensating`
  ([execution stream](../09-api/workflows.md#6-execution-lifecycle)).
- Inspect per-node inputs/outputs, retries, and logs.
- Complete [human tasks](../09-api/workflows.md#8-human-tasks) inline.

---

# 8. Templates & Reuse

- Start from templates (e.g. approval, RAG pipeline, ETL).
- Extract a selection into a reusable sub-workflow.
- Import/export DSL for version control and code review.

---

# 9. Collaboration

- Comments on nodes.
- Draft sharing within a project (RBAC-scoped).
- Optimistic concurrency on save via [`ETag`/`If-Match`](../09-api/overview.md#10-concurrency-control);
  conflicting edits prompt a merge/reload.

---

# 10. Accessibility

The canvas supports keyboard-driven node creation/navigation and screen-reader
labels for nodes and edges, in line with
[Overview §11](overview.md#11-accessibility--i18n).

---

# 11. Dependencies

- [`03-workflow-engine/workflow-dsl.md`](../03-workflow-engine/workflow-dsl.md)
- [`09-api/workflows.md`](../09-api/workflows.md)
- [`09-api/tools.md`](../09-api/tools.md)

---

# 12. Related Documents

- [`10-dashboard/overview.md`](overview.md)
- [`10-dashboard/agent-studio.md`](agent-studio.md)
- [`10-dashboard/monitoring.md`](monitoring.md)

---

# 13. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Workflow Builder specification |
