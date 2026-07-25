<!--
File: docs/01-product/functional-requirements.md
Document ID: PRD-004
-->

# Functional Requirements

**Document ID:** PRD-004  
**File Path:** `docs/01-product/functional-requirements.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Product Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document enumerates the **functional requirements (FRs)** for the Wovyr AI
Platform — what the system must *do*. FRs are derived from the [PRD](prd.md#12-functional-overview)
and [user stories](user-stories.md), and each points to the spec that realizes it.

---

# 2. Conventions

```text
FR-<area>-<n>  ·  Priority: MUST | SHOULD | MAY  ·  Spec: <link>
```

---

# 3. Agents

| ID | Requirement | Pri | Spec |
|----|-------------|-----|------|
| FR-AGT-1 | Define agents declaratively (model, instructions, tools, memory, policies) | MUST | [agent-definition](../04-agent-framework/agent-definition.md) |
| FR-AGT-2 | Execute an agent against a goal/input | MUST | [agent-runtime-protocol](../04-agent-framework/agent-runtime-protocol.md) |
| FR-AGT-3 | Stream run progress (planner, tools, model, memory) | MUST | [Agents API §6](../09-api/agents.md#6-run-lifecycle--streaming) |
| FR-AGT-4 | Maintain multi-turn sessions | SHOULD | [Agents API §7](../09-api/agents.md#7-sessions) |
| FR-AGT-5 | Version and publish agents | MUST | [Agents API §8](../09-api/agents.md#8-versioning--publishing) |

---

# 4. Workflows

| ID | Requirement | Pri | Spec |
|----|-------------|-----|------|
| FR-WF-1 | Author workflows in a DSL (YAML/JSON/visual/SDK) | MUST | [workflow-dsl](../03-workflow-engine/workflow-dsl.md) |
| FR-WF-2 | Execute durably with checkpoint/resume | MUST | [checkpointing](../03-workflow-engine/checkpointing-specification.md) |
| FR-WF-3 | Support human-approval tasks | MUST | [Workflows API §8](../09-api/workflows.md#8-human-tasks) |
| FR-WF-4 | Support retries and compensation (saga) | MUST | [retry](../03-workflow-engine/retry-engine.md) / [compensation](../03-workflow-engine/compensation-engine.md) |
| FR-WF-5 | Branching, parallelism, loops, sub-workflows | MUST | [workflow-dsl](../03-workflow-engine/workflow-dsl.md) |
| FR-WF-6 | Event/timer-driven execution | SHOULD | [Workflows API §7](../09-api/workflows.md#7-signals--events) |

---

# 5. Memory

| ID | Requirement | Pri | Spec |
|----|-------------|-----|------|
| FR-MEM-1 | Store/version/retrieve memory records | MUST | [memory-api](../06-memory-engine/memory-api.md) |
| FR-MEM-2 | Hybrid (vector+keyword+graph) retrieval with ranking | MUST | [retrieval](../06-memory-engine/retrieval.md) / [ranking](../06-memory-engine/ranking.md) |
| FR-MEM-3 | Scope and access-control memory per tenant/project | MUST | [memory scopes](../06-memory-engine/memory-api.md#10-scopes--sharing) |
| FR-MEM-4 | Compress retrieved context to a token budget | SHOULD | [compression](../06-memory-engine/compression.md) |

---

# 6. LLM Gateway

| ID | Requirement | Pri | Spec |
|----|-------------|-----|------|
| FR-LLM-1 | Provider-neutral inference (chat/embeddings/etc.) | MUST | [provider-api](../05-llm-gateway/provider-api.md) |
| FR-LLM-2 | Routing, failover, resilience | MUST | [routing](../05-llm-gateway/routing.md) / [resilience](../05-llm-gateway/resilience.md) |
| FR-LLM-3 | Token accounting, budgets, cost events | MUST | [token-management](../05-llm-gateway/token-management.md) |
| FR-LLM-4 | Response caching | SHOULD | [caching](../05-llm-gateway/caching.md) |

---

# 7. Tools

| ID | Requirement | Pri | Spec |
|----|-------------|-----|------|
| FR-TOOL-1 | Execute tools in isolated sandboxes with limits | MUST | [tool-runtime](../07-tool-runtime/index.md) |
| FR-TOOL-2 | Discover/register/version tools | MUST | [tool-framework](../04-agent-framework/tool-framework.md) |
| FR-TOOL-3 | Enforce per-tool permissions and egress control | MUST | [security-isolation](../07-tool-runtime/security-isolation.md) |
| FR-TOOL-4 | Provide a built-in tool catalog (fs/shell/http/git/db/…) | SHOULD | [tool catalog](../07-tool-runtime/filesystem.md) |

---

# 8. Plugins

| ID | Requirement | Pri | Spec |
|----|-------------|-----|------|
| FR-PLG-1 | Author/package/sign plugins | MUST | [plugin-api](../08-plugin-sdk/plugin-api.md) / [distribution](../08-plugin-sdk/distribution.md) |
| FR-PLG-2 | Install/version/upgrade plugins at runtime | MUST | [versioning](../08-plugin-sdk/versioning.md) |
| FR-PLG-3 | Declare and grant least-privilege permissions | MUST | [permissions](../08-plugin-sdk/permissions.md) |
| FR-PLG-4 | Marketplace discovery + governance | SHOULD | [marketplace](../08-plugin-sdk/marketplace.md) |

---

# 9. API, Security & Tenancy

| ID | Requirement | Pri | Spec |
|----|-------------|-----|------|
| FR-API-1 | REST + gRPC management API (`/v1`) | MUST | [API overview](../09-api/overview.md) |
| FR-SEC-1 | AuthN (OAuth2/JWT/API key/mTLS) | MUST | [authentication](../13-security/authentication.md) |
| FR-SEC-2 | AuthZ via RBAC + ABAC, fail-closed | MUST | [authorization](../13-security/authorization.md) |
| FR-SEC-3 | Encryption in transit & at rest; secret vault | MUST | [encryption](../13-security/encryption.md) / [secrets](../13-security/secret-management.md) |
| FR-SEC-4 | Tamper-evident audit of sensitive actions | MUST | [audit](../13-security/audit.md) |
| FR-ADM-1 | Manage orgs/projects/users/quotas | MUST | [projects](../09-api/projects.md) / [users](../09-api/users.md) |

---

# 10. Dashboard, CLI & Observability

| ID | Requirement | Pri | Spec |
|----|-------------|-----|------|
| FR-UI-1 | Web dashboard for build/operate/monitor | MUST | [dashboard](../10-dashboard/index.md) |
| FR-DX-1 | CLI with API parity + local mode | MUST | [CLI](../11-cli/index.md) |
| FR-DX-2 | Rust SDK + scaffolding | SHOULD | [build-system §6](../19-implementation-guide/build-system.md#6-the-rust-sdk) |
| FR-OBS-1 | Logs, metrics, traces, dashboards, alerts | MUST | [observability](../14-observability/index.md) |

---

# 11. Traceability

Each FR links to [user stories](user-stories.md) and is validated by
[acceptance criteria](acceptance-criteria.md) and [tests](../15-testing/index.md),
per [PRD §23](prd.md#23-traceability).

---

# 12. Related

- [`01-product/prd.md`](prd.md) · [`01-product/non-functional-requirements.md`](non-functional-requirements.md)

---

# 13. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Functional Requirements |
