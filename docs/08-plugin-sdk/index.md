<!--
File: docs/08-plugin-sdk/index.md
Document ID: PLG-INDEX-001
-->

# Plugin SDK Index

**Document ID:** PLG-INDEX-001  
**File Path:** `docs/08-plugin-sdk/index.md`  
**Version:** 1.0.0  
**Status:** Active  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document is the **central navigation and architecture index** for the Plugin SDK in the Wovyr AI Platform.

Wovyr is **Plugin First**: every capability that can be a plugin should be one (see [Vision §Plugin First](../00-executive/vision.md)). The Plugin SDK is how developers build those capabilities, and the **Plugin Engine** is the service that installs, versions, governs, and loads them at runtime.

---

# 2. What Is a Plugin

A **plugin** is a versioned, signed, deployable package that contributes one or more **capabilities** to the platform without recompiling it.

| | |
|---|---|
| **Plugin** | Distributable package (manifest + artifacts + capabilities) |
| **Capability** | A single extension a plugin provides (a tool, a provider, …) |
| **Plugin SDK** | Authoring library + manifest format for building plugins |
| **Plugin Engine** | Deployable service managing plugin lifecycle (c4-container §4.7) |
| **Marketplace** | Distribution + discovery surface for plugins |

A plugin packaging *tools* reuses the [Tool Framework](../04-agent-framework/tool-framework.md#48-plugin-architecture)
package format; the Plugin SDK generalizes that to all capability kinds.

---

# 3. Capability Kinds

Plugins can extend the platform at every documented extension point
([Agent Framework §12](../04-agent-framework/index.md)):

| Capability kind | Extends | Runs in |
|-----------------|---------|---------|
| `tool` | Tool catalog | [Tool Runtime](../07-tool-runtime/index.md) (sandboxed) |
| `provider` | LLM providers | [LLM Gateway](../05-llm-gateway/index.md) |
| `memory_backend` | Storage/retrieval | [Memory Engine](../06-memory-engine/index.md) |
| `planner_strategy` | Planning | [Planning Engine](../04-agent-framework/planning-engine.md) |
| `policy` | Governance rules | [Policy Engine](../04-agent-framework/policy-engine.md) |
| `workflow_activity` | DSL activities | [Workflow Engine](../03-workflow-engine/overview.md) |
| `agent_type` / `coordination` | Agent behaviors | [Agent Runtime](../04-agent-framework/agent-runtime-protocol.md) |

One plugin may bundle several capabilities (e.g. a "GitHub" plugin shipping tools
+ a workflow activity).

---

# 4. SDK vs. Engine

As with the platform's other service/abstraction splits, authoring is separated
from operation.

| Concern | Plugin SDK | Plugin Engine |
|---------|-----------|---------------|
| Form | Library + manifest + CLI | Deployable service / container |
| Audience | Plugin developers | Operators + the platform runtime |
| Responsibility | Define capabilities, manifest, build/package | Install, verify, version, load, govern |
| Output | A signed plugin package | A live, registered capability |

---

# 5. Plugin Lifecycle (High Level)

```text
Author (SDK) ─► Build & sign ─► Publish (Marketplace/registry)
                                      │
                                      ▼
                              Install (Plugin Engine)
                                      │
            verify signature ─► resolve dependencies ─► check compatibility
                                      │
                                      ▼
                         Register capabilities ─► Enable
                                      │
                                      ▼
            Load on demand ─► route to host (Tool Runtime / Gateway / …)
                                      │
                         Upgrade / Disable / Rollback / Uninstall
```

Detailed in [Versioning & Lifecycle](versioning.md).

---

# 6. Document Map

| Document | Responsibility |
|----------|----------------|
| [overview.md](overview.md) | Plugin system + Plugin Engine architecture, NFRs |
| [plugin-api.md](plugin-api.md) | SDK: traits, manifest, capability registration |
| [permissions.md](permissions.md) | Plugin permission model, capability grants, consent |
| [sandbox.md](sandbox.md) | Plugin isolation and execution surfaces |
| [versioning.md](versioning.md) | Semver, compatibility, dependency resolution, lifecycle |
| [distribution.md](distribution.md) | Packaging, signing, registry, provenance |
| [marketplace.md](marketplace.md) | Discovery, publishing, ratings, governance, monetization |

---

# 7. Design Principles

1. **Plugin First** — capabilities are plugins unless they must be core.
2. **Declared, least-privilege permissions** — a plugin gets only what it requests and is granted.
3. **Isolated by default** — untrusted plugin code runs sandboxed.
4. **Versioned & compatible** — semver with explicit platform API ranges.
5. **Signed & provenant** — every package is verifiable end to end.
6. **Hot lifecycle** — install/enable/disable/upgrade without platform restart.
7. **Observable** — install and load emit `plugin.*` events and audit.

---

# 8. Dependencies

- [`04-agent-framework/tool-framework.md`](../04-agent-framework/tool-framework.md#48-plugin-architecture) — tool plugin packaging
- [`07-tool-runtime/index.md`](../07-tool-runtime/index.md) — executes tool plugins
- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md) — permission enforcement
- [`02-architecture/event-driven-architecture.md`](../02-architecture/event-driven-architecture.md) — `plugin.*` events

---

# 9. Related Documents

- [`02-architecture/c4-container.md`](../02-architecture/c4-container.md) — Plugin Engine container (§4.7)
- [`02-architecture/domain-driven-design.md`](../02-architecture/domain-driven-design.md) — Plugin Framework domain
- [`01-product/system-overview.md`](../01-product/system-overview.md) — Plugin Framework overview

---

# 10. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Plugin SDK Index |
