<!--
File: docs/10-dashboard/overview.md
Document ID: DASH-001
-->

# Dashboard Overview & Architecture

**Document ID:** DASH-001  
**File Path:** `docs/10-dashboard/overview.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

 > **Implementation status (v0.3, 2026-06-30).** The first dashboard slice ships the
> **Angular SPA only**, talking **directly to `apex-server`** (the Rust platform API)
> — the NestJS **BFF is deferred**. This is sound because `apex-server` already serves
> the `/api/v1` REST surface and native **SSE** (`agents:stream`), which the browser
> consumes directly (`fetch` streaming), so no Node WebSocket bridge is needed yet. In
> development the SPA reaches the API through the Angular dev-server proxy
> (`dashboard/proxy.conf.json` → `127.0.0.1:8080`); in production it is same-origin
> behind the gateway. The BFF's responsibilities below (server-side sessions /
> OAuth2-PKCE, view aggregation, stream fan-in, CSRF) are **deferred until production
> auth is needed**, at which point they will be added either as a thin Node tier or
> folded into `apex-server`. The target architecture in §3–§7 remains the goal; the
> sections below describe that end state. First built surface: **Agent Studio**
> (`dashboard/src/app/features/agent-studio`). Stack deviation tracked here and in
> [`index.md`](index.md); the documented Angular UI is unchanged.

# 1. Purpose

This document specifies the architecture of the Apex AI Platform **Dashboard**: the Angular single-page application (SPA), the NestJS backend-for-frontend (BFF), how they consume the [Platform API](../09-api/index.md), and the cross-cutting concerns (auth, real-time, RBAC-driven UI, theming, observability).

---

# 2. Scope

The Dashboard is responsible for:

- Authenticated, RBAC-aware web access to all platform resources
- Visual authoring (workflows, agents)
- Exploration (memory, marketplace)
- Operations (monitoring, cost, administration)
- Real-time views of runs and executions

It is **not** responsible for:

- Business logic or persistence — that lives behind the [Platform API](../09-api/index.md)
- Authorization decisions — enforced by the API + [Policy Engine](../04-agent-framework/policy-engine.md)
- Executing agents/workflows/tools — those run in their respective services

---

# 3. Architecture

```text
Browser (Angular SPA)
   │  HTTPS / WSS
   ▼
Dashboard Backend (NestJS BFF)
   │  REST / gRPC / WebSocket
   ▼
API Gateway ──► platform services
```

- **Angular SPA** renders the UI and holds view state.
- **NestJS BFF** handles browser sessions, aggregates multiple API calls into
  view-shaped responses, terminates websockets, and serves static assets. It is
  stateless and horizontally scalable (c4-container §4.9).
- The BFF **forwards the user's identity** to the API Gateway; it never holds
  elevated privilege.

---

# 4. Why a BFF

| Concern | Handled by BFF |
|---------|----------------|
| Browser session ↔ token exchange | OAuth2 Authorization Code + PKCE |
| Aggregation | Combine several API calls into one view payload |
| Streaming fan-in | Bridge platform SSE/gRPC streams to browser WebSockets |
| CSRF/cookie security | Secure, same-site session cookies |
| Caching | Short-lived caching of reference data |

The BFF keeps the SPA simple and avoids exposing long-lived tokens to the browser.

---

# 5. Authentication Flow

```text
1. User clicks "Sign in"
2. SPA → BFF → IdP (OAuth2 Authorization Code + PKCE / OIDC SSO)
3. BFF exchanges code for tokens; stores them server-side
4. BFF issues a secure, http-only session cookie to the browser
5. Subsequent SPA calls carry the cookie; BFF attaches the access token to API calls
6. Token refresh handled by the BFF transparently
```

Aligned with [API Authentication §3](../09-api/authentication.md#3-oauth2--oidc).
Access tokens never live in browser storage.

---

# 6. RBAC-Driven UI

The UI adapts to the user's effective scopes (from
[`GET /users/me`](../09-api/users.md#10-self-service)):

- Navigation items and actions are hidden/disabled when the scope is absent.
- The UI is a *convenience* filter only — the API remains the enforcement point
  (a hidden button is still denied server-side if called).
- Tenant/project switchers constrain every view to the active scope.

---

# 7. Real-Time

```text
Agent run / workflow execution / metrics
        │  platform SSE / gRPC stream
        ▼
   NestJS BFF (stream bridge)
        │  WebSocket
        ▼
   Angular SPA (live views)
```

Live surfaces include agent runs ([Agents API §6](../09-api/agents.md#6-run-lifecycle--streaming)),
workflow executions ([Workflows API §6](../09-api/workflows.md#6-execution-lifecycle)),
tool executions, and monitoring metrics. The SPA reconnects with backoff and
resumes from the last received sequence where supported.

---

# 8. Frontend Structure

```text
dashboard-ui/ (Angular)
├── core/            # auth, http interceptors, error handling
├── shared/          # design system, components, directives
├── features/
│   ├── monitoring/
│   ├── workflow-builder/
│   ├── agent-studio/
│   ├── memory-explorer/
│   ├── marketplace/
│   └── settings/
├── state/           # state management (signals/store)
└── app.routes.ts
```

Features are lazy-loaded routes. A shared **design system** enforces consistent
components, theming (light/dark), and accessibility.

---

# 9. State Management

- Server state is fetched via typed API clients and cached with invalidation on
  domain events (received over the websocket bridge).
- UI state (selections, drafts) is local to features.
- Optimistic updates use the API's [concurrency control](../09-api/overview.md#10-concurrency-control)
  (`ETag`/`If-Match`) and reconcile on conflict.

---

# 10. Error Handling

- API errors use the [standard envelope](../09-api/overview.md#8-error-model); the
  UI maps `code`/`type` to friendly messages and surfaces the `request_id` for
  support.
- `403` triggers a permission-explained state, not a dead end.
- `429` shows backoff/retry affordances using `Retry-After`.

---

# 11. Accessibility & i18n

- Targets WCAG 2.1 AA: keyboard navigation, ARIA, contrast, focus management.
- All strings externalized for localization; RTL-ready layouts.

---

# 12. Performance

| Concern | Approach |
|---------|----------|
| Initial load | Lazy routes, code splitting, SSR/prerender for shell |
| Large lists | Virtual scrolling + cursor pagination |
| Live updates | Diff-based DOM updates from streamed deltas |
| Bundle size | Tree-shaking, on-demand feature loading |

---

# 13. Security

- No long-lived tokens in the browser (server-side session).
- Strict CSP, same-site cookies, CSRF protection at the BFF.
- The dashboard inherits all platform authorization; it cannot exceed the user's
  granted scopes.

---

# 14. Observability

The dashboard emits anonymized usage telemetry (feature usage, error rates, load
times) and propagates `request_id`/trace context to the API for end-to-end tracing.
Dashboard adoption is a tracked [success metric](../00-executive/success-metrics.md).

---

# 15. Dependencies

- [`09-api/index.md`](../09-api/index.md)
- [`09-api/authentication.md`](../09-api/authentication.md)
- [`02-architecture/c4-container.md`](../02-architecture/c4-container.md)

---

# 16. Related Documents

- [`10-dashboard/workflow-builder.md`](workflow-builder.md)
- [`10-dashboard/agent-studio.md`](agent-studio.md)
- [`10-dashboard/monitoring.md`](monitoring.md)
- [`10-dashboard/settings.md`](settings.md)

---

# 17. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Dashboard Overview & Architecture |
| 1.1.0 | 2026-06-30 | Implementation status: first slice ships the Angular SPA directly against apex-server; NestJS BFF deferred until production auth. Agent Studio built first. |
