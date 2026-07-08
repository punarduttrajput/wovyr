<!--
File: docs/01-product/user-stories.md
Document ID: PRD-008
-->

# User Stories

**Document ID:** PRD-008  
**File Path:** `docs/01-product/user-stories.md`  
**Version:** 1.0.1  
**Status:** Draft  
**Owner:** Product Team  
**Last Updated:** 2026-07-07

---

# 1. Purpose

This document captures **user stories** — concrete needs in the `As a … I want … so that …` form — grouped by epic. They translate [personas](personas.md) and the [PRD](prd.md) into work items, and map to [functional requirements](functional-requirements.md) and [acceptance criteria](acceptance-criteria.md).

---

# 2. Format

```text
US-<area>-<n>: As a <persona>, I want <capability>, so that <outcome>.
   → FRs: <ids>   → Acceptance: <ids>
```

---

# 3. Agents

- **US-AGT-1**: As an *AI Engineer*, I want to define an agent declaratively, so that
  I can version and review it. → FR-AGT-1 → AC-AGT-1
- **US-AGT-2**: As an *App Developer*, I want to run an agent and stream its steps,
  so that I can see tool calls and reasoning. → FR-AGT-2,3 → AC-AGT-2
- **US-AGT-3**: As an *AI Engineer*, I want a session across turns, so that context
  persists in a conversation. → FR-AGT-4 → AC-AGT-3

(See [Agents API](../09-api/agents.md), [Agent Studio](../10-dashboard/agent-studio.md).)

---

# 4. Workflows

- **US-WF-1**: As an *AI Engineer*, I want to author workflows visually or in DSL, so
  that I can orchestrate multi-step processes. → FR-WF-1 → AC-WF-1
- **US-WF-2**: As an *Ops Engineer*, I want durable executions that survive restarts,
  so that long-running work is reliable. → FR-WF-2 → AC-WF-2
- **US-WF-3**: As a *Manager*, I want human-approval steps, so that high-risk actions
  need sign-off. → FR-WF-3 → AC-WF-3
- **US-WF-4**: As an *AI Engineer*, I want compensation on failure, so that partial
  work rolls back. → FR-WF-4 → AC-WF-4

(See [Workflow Engine](../03-workflow-engine/overview.md), [Workflow Builder](../10-dashboard/workflow-builder.md).)

---

# 5. Memory

- **US-MEM-1**: As an *AI Engineer*, I want to store knowledge and retrieve it
  semantically, so that agents answer from my data. → FR-MEM-1,2 → AC-MEM-1
- **US-MEM-2**: As a *Security* user, I want memory scoped and access-controlled, so
  that data stays isolated. → FR-MEM-3 → AC-MEM-2

(See [Memory Engine](../06-memory-engine/index.md).)

---

# 6. Tools & Plugins

- **US-TOOL-1**: As an *AI Engineer*, I want agents to call tools safely, so that they
  act on the world without risk. → FR-TOOL-1 → AC-TOOL-1
- **US-PLG-1**: As a *Plugin Developer*, I want to package and publish a capability,
  so that others can install it. → FR-PLG-1,2 → AC-PLG-1
- **US-PLG-2**: As a *Security* user, I want to approve plugin permissions, so that
  extensions get least privilege. → FR-PLG-3 → AC-PLG-2

(See [Tool Runtime](../07-tool-runtime/index.md), [Plugin SDK](../08-plugin-sdk/index.md).)

---

# 7. LLM Gateway

- **US-LLM-1**: As an *App Developer*, I want to switch providers without code change,
  so that I avoid lock-in. → FR-LLM-1 → AC-LLM-1
- **US-LLM-2**: As an *Ops Engineer*, I want failover and cost tracking, so that
  inference is resilient and budgeted. → FR-LLM-2,3 → AC-LLM-2

(See [LLM Gateway](../05-llm-gateway/index.md).)

---

# 8. Platform, Security & Ops

- **US-SEC-1**: As a *Security* user, I want RBAC + audit, so that access is governed
  and traceable. → FR-SEC-1,2 → AC-SEC-1
- **US-OPS-1**: As an *Ops Engineer*, I want metrics, traces, and alerts, so that I
  can operate to SLOs. → FR-OBS-1 → AC-OBS-1
- **US-ADM-1**: As an *Admin*, I want projects, quotas, and users, so that teams
  self-serve under limits. → FR-ADM-1 → AC-ADM-1

(See [Security](../13-security/index.md), [Observability](../14-observability/index.md),
[Projects](../09-api/projects.md).)

---

# 9. Developer Experience

- **US-DX-1**: As an *App Developer*, I want a CLI with local mode, so that I can
  build and test offline. → FR-DX-1 → AC-DX-1
- **US-DX-2**: As a *Plugin Developer*, I want SDK scaffolding and tests, so that I
  build plugins quickly. → FR-DX-2 → AC-PLG-1

(See [CLI](../11-cli/index.md), [SDK](../19-implementation-guide/build-system.md#6-the-rust-sdk).)

---

# 10. Traceability

Each story links to [functional requirements](functional-requirements.md) and
[acceptance criteria](acceptance-criteria.md); per the
[PRD §23](prd.md#23-traceability), implementation artifacts trace back through these.

---

# 11. Related

- [`01-product/personas.md`](personas.md)
- [`01-product/functional-requirements.md`](functional-requirements.md)
- [`01-product/acceptance-criteria.md`](acceptance-criteria.md)

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.1 | 2026-07-07 | Renumbered from PRD-003 to PRD-008 — that ID collided with [`prd-ga-hardening.md`](prd-ga-hardening.md), which was independently assigned PRD-003 later without checking the sequence. Found during a project-wide doc review; no content changed |
| 1.0.0 | 2026-06-27 | Initial User Stories |
