<!--
File: docs/10-dashboard/index.md
Document ID: DASH-INDEX-001
-->

# Dashboard Index

**Document ID:** DASH-INDEX-001  
**File Path:** `docs/10-dashboard/index.md`  
**Version:** 1.0.0  
**Status:** Active  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document is the **central navigation and architecture index** for the Wovyr AI Platform Dashboard — the web application through which users design agents and workflows, explore memory, manage plugins, and monitor the platform.

The Dashboard is a **client of the [Platform API](../09-api/index.md)**: it adds no
privileged capability of its own. Everything it does is an authenticated,
authorized API call.

---

# 2. Composition

| Layer | Technology | Role |
|-------|-----------|------|
| Dashboard UI | Angular | Single-page application (c4-container §4.9). **Built** — `dashboard/`, first surface: Agent Studio. |
| Dashboard Backend | NestJS | Backend-for-frontend (BFF): session, aggregation, websockets. **Deferred** — see below. |
| Platform API | REST/gRPC | Source of truth for all resources (`wovyr-server`). |

> **Status (v0.3):** the SPA currently calls **`wovyr-server` directly** (it already
> serves `/api/v1` + SSE), so the **NestJS BFF is deferred** until production auth
> (server-side sessions / OAuth2-PKCE) and view aggregation are needed. See
> [`overview.md`](overview.md) for the rationale and the deferred BFF responsibilities.

When introduced, the BFF never bypasses platform authorization — it forwards the
user's identity to the [Platform API](../09-api/index.md) and the
[Policy Engine](../04-agent-framework/policy-engine.md) enforces access. Until then,
the same enforcement happens at `wovyr-server`.

---

# 3. Surfaces

```text
Dashboard
│
├── Home / Monitoring     (health, runs, cost)
├── Workflow Builder      (visual DSL editor)
├── Agent Studio          (build + test agents)
├── Memory Explorer       (browse/search memory)
├── Marketplace           (discover + install plugins)
└── Settings              (orgs, projects, users, keys)
```

---

# 4. Document Map

| Document | Responsibility |
|----------|----------------|
| [overview.md](overview.md) | UI/BFF architecture, API consumption, real-time, RBAC-driven UI |
| [workflow-builder.md](workflow-builder.md) | Visual workflow studio (DSL ⇄ canvas) |
| [agent-studio.md](agent-studio.md) | Authoring, testing, and observing agents |
| [memory-explorer.md](memory-explorer.md) | Browsing, searching, and managing memory |
| [marketplace.md](marketplace.md) | Plugin discovery, install, and grants UI |
| [monitoring.md](monitoring.md) | Operational + cost dashboards |
| [settings.md](settings.md) | Tenancy, identity, and key administration |

---

# 5. Design Principles

1. **API-only** — the UI is a thin client over the [Platform API](../09-api/index.md).
2. **RBAC-driven** — the UI shows only what the user's scopes permit.
3. **Real-time** — runs, executions, and metrics stream live.
4. **Multi-tenant aware** — every view is tenant/project scoped.
5. **Accessible & responsive** — WCAG-compliant, works across devices.
6. **Observable** — the dashboard itself emits usage telemetry.

---

# 6. Dependencies

- [`09-api/index.md`](../09-api/index.md) — the API the Dashboard consumes
- [`09-api/authentication.md`](../09-api/authentication.md) — login + RBAC
- [`02-architecture/c4-container.md`](../02-architecture/c4-container.md) — Dashboard containers

---

# 7. Related Documents

- [`00-executive/vision.md`](../00-executive/vision.md) — Visual Studio vision
- [`11-cli`](../SUMMARY.md) *(planned: the CLI is the other primary client)*

---

# 8. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Dashboard Index |
