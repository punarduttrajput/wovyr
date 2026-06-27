<!--
File: docs/01-product/acceptance-criteria.md
Document ID: PRD-006
-->

# Acceptance Criteria

**Document ID:** PRD-006  
**File Path:** `docs/01-product/acceptance-criteria.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Product Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines **acceptance criteria (AC)** — the verifiable conditions that
confirm a [functional requirement](functional-requirements.md) or
[user story](user-stories.md) is satisfied. ACs are written so they can be turned
directly into [tests](../15-testing/index.md).

---

# 2. Format

```text
AC-<area>-<n> (verifies US/FR): Given <context>, when <action>, then <observable outcome>.
```

---

# 3. Agents

- **AC-AGT-1** (FR-AGT-1): Given a valid agent definition, when created, then it is
  stored, versioned, and retrievable; an invalid definition is rejected with a
  schema error.
- **AC-AGT-2** (FR-AGT-2/3): Given a published agent, when run with input, then a
  result is returned and the stream shows planner steps, tool calls, and model
  deltas ([Agents API](../09-api/agents.md#6-run-lifecycle--streaming)).
- **AC-AGT-3** (FR-AGT-4): Given a session, when a second run references it, then
  prior conversation context is available.

---

# 4. Workflows

- **AC-WF-1** (FR-WF-1): Given a DSL definition, when validated, then errors
  (unreachable nodes, bad expressions) are reported before publish.
- **AC-WF-2** (FR-WF-2): Given a running execution, when the engine restarts, then it
  resumes from its last checkpoint and completes deterministically.
- **AC-WF-3** (FR-WF-3): Given a human-task step, when reached, then the execution
  suspends until the task is completed, then resumes.
- **AC-WF-4** (FR-WF-4): Given a failure after a compensable step, when triggered,
  then the configured compensation runs and reverses the effect.

---

# 5. Memory

- **AC-MEM-1** (FR-MEM-1/2): Given stored knowledge, when queried semantically, then
  relevant records return ranked with score breakdowns; an out-of-corpus query
  returns nothing relevant.
- **AC-MEM-2** (FR-MEM-3): Given records in another tenant, when queried, then they
  are never returned (zero cross-tenant leakage).

---

# 6. LLM Gateway

- **AC-LLM-1** (FR-LLM-1): Given a request by capability, when the configured
  provider changes, then the same request succeeds without caller code changes.
- **AC-LLM-2** (FR-LLM-2/3): Given a primary provider failure, when a request is
  made, then it fails over to a healthy provider and the response reports tokens and
  cost.

---

# 7. Tools & Plugins

- **AC-TOOL-1** (FR-TOOL-1/3): Given a tool with no egress grant, when it attempts a
  network call, then it is blocked and audited; resource-limit breaches kill the
  sandbox.
- **AC-PLG-1** (FR-PLG-1/2): Given a signed plugin, when installed, then signature
  and provenance are verified; an unsigned/tampered package is rejected.
- **AC-PLG-2** (FR-PLG-3): Given a plugin requesting a permission, when not granted,
  then the dependent capability does not run.

---

# 8. Security & Tenancy

- **AC-SEC-1** (FR-SEC-2): Given a principal lacking a scope, when calling a
  protected endpoint, then the request is denied (403) and audited; auth errors
  fail closed.
- **AC-ADM-1** (FR-ADM-1): Given a project quota, when exceeded, then further
  operations are rejected (429/402) with a clear error.

---

# 9. Observability & DX

- **AC-OBS-1** (FR-OBS-1): Given any request, when processed, then logs, a trace, and
  metrics are emitted sharing one `request_id`/`trace_id`.
- **AC-DX-1** (FR-DX-1): Given the CLI in `--local` mode, when an agent is run, then
  it executes without a server and returns a result.

---

# 10. Release Gating

A release is accepted when all **MUST** FRs have passing ACs (as automated tests),
NFR targets are met ([performance](../15-testing/performance-tests.md)), and security
ACs pass ([security testing](../15-testing/security-testing.md)) — aligned with the
[roadmap exit criteria](../18-roadmap/v0.1.md#5-exit-criteria).

---

# 11. Related

- [`01-product/functional-requirements.md`](functional-requirements.md)
- [`01-product/user-stories.md`](user-stories.md)
- [`15-testing/index.md`](../15-testing/index.md)

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Acceptance Criteria |
