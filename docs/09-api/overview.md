<!--
File: docs/09-api/overview.md
Document ID: API-001
-->

# Platform API Overview & Conventions

**Document ID:** API-001  
**File Path:** `docs/09-api/overview.md`  
**Version:** 1.4.0  
**Status:** Draft — this document describes the **target-state** convention;
the machine-readable, ground-truth contract for what `apex-server` actually
implements today is served live at `GET /openapi.json` (RM-AIM-P3 SRV-303) —
generated at compile time from `#[utoipa::path(...)]` annotations on the
handlers themselves plus `#[derive(ToSchema)]` request/error types (see
`crates/apex-server/src/openapi.rs`), so it cannot silently drift from the
code the way a hand-maintained file can; the CI contract-gate job lints this
live document, not a checked-in copy. [`openapi.yaml`](openapi.yaml) (hand-
authored from the Axum routes, v1.0 "Stability" deliverable) remains as a
browsable, checked-in snapshot but is no longer the thing CI validates
against — treat a divergence between the two as the hand-authored file being
stale, not the generated one. Notable gaps between this doc's target-state
convention and the real API (both `openapi.yaml` and the generated doc): the
real API has no opaque `agt_01H...`-style ids (resources use their natural
key — agent name, workflow `execution_id`, `publisher/name`, …), no OAuth2
authorization-code flow and no mTLS, and no generic `/operations/{id}`
polling resource. **Authentication is real, not a placeholder** (corrected
2026-07-07 — this line previously said "no OAuth2/JWT," which stopped being
true once RM-GA-P1 SEC-101 landed): `APEX_AUTH_MODE=jwt` (HS256/RS256 bearer)
or `apikey` (a hashed bearer token) verify the caller before any handler
runs, overwriting whatever `X-Apex-Principal` the client sent; the
`disabled-loopback` default (plain, unverified `X-Apex-Tenant`/
`X-Apex-Principal` headers) is a local-dev fallback, not the production
posture, and its anonymous escape hatch refuses to bind non-loopback. See
[`13-security/authentication.md`](../13-security/authentication.md) and
`crates/apex-server/src/auth.rs`. Pagination, the `Idempotency-Key` header
(now on every mutating route, not just `agents:run` — RM-GA-P4 API-703),
`If-Match`/`ETag` concurrency, and the error envelope below *are* implemented
as documented. A TypeScript client generated against `openapi.yaml` lives
at [`sdks/typescript`](../../sdks/typescript), with retry/backoff on `GET`
requests and a `paginateAll()` auto-iteration helper. The deprecation window
this section's `/v2` sentence promises is spelled out concretely in
[deprecation-policy.md](deprecation-policy.md).  
**Owner:** AI Platform Team  
**Last Updated:** 2026-07-07

---

# 1. Purpose

This document defines the **conventions** shared by every Apex AI Platform API endpoint: protocols, versioning, resource naming, pagination, filtering, errors, idempotency, rate limiting, and observability. Resource-specific documents (agents, workflows, …) inherit these rules.

---

# 2. Protocols

| Protocol | Use |
|----------|-----|
| REST (HTTP/JSON) | Primary; all resources |
| gRPC | Equivalent semantics for internal/high-performance clients |
| WebSocket / SSE | Streaming (logs, runs, events) |

All three are served through the [API Gateway](../02-architecture/c4-container.md).
REST is the reference; gRPC mirrors it method-for-method.

---

# 3. Base URL & Versioning

```text
https://{host}/api/v1/...
```

- The API is namespaced `/api/v1`.
- Additive changes (new fields, new endpoints) are backward compatible.
- Breaking changes introduce `/api/v2`, run in parallel, and follow a published
  deprecation window — see [deprecation-policy.md](deprecation-policy.md) for
  the concrete window (90 days minimum) and required headers.
- Responses include an `Apex-Api-Version` header.

---

# 4. Resource Conventions

| Verb | Pattern | Semantics |
|------|---------|-----------|
| `GET /resources` | List | Paginated collection |
| `POST /resources` | Create | Create one resource |
| `GET /resources/{id}` | Read | Fetch one |
| `PATCH /resources/{id}` | Update | Partial update |
| `PUT /resources/{id}` | Replace | Full replace |
| `DELETE /resources/{id}` | Delete | Remove (soft by default) |
| `POST /resources/{id}:action` | Action | Non-CRUD verb (e.g. `:run`, `:cancel`) |

Resource IDs are opaque, prefixed strings (e.g. `agt_01H...`, `wf_01H...`).

---

# 5. Standard Resource Envelope

```json
{
  "id": "agt_01H...",
  "object": "agent",
  "tenant": "acme",
  "project": "support-bot",
  "created_at": "2026-06-27T10:00:00Z",
  "updated_at": "2026-06-27T10:00:00Z",
  "version": 3
}
```

Every resource carries `id`, `object`, tenant/project scoping, timestamps, and a
`version` for optimistic concurrency (via `If-Match` / `ETag`).

---

# 6. Pagination

Cursor-based pagination is the default:

```http
GET /api/v1/agents?limit=50&cursor=eyJvZmZzZXQiOjUwfQ
```

```json
{
  "data": [ ... ],
  "has_more": true,
  "next_cursor": "eyJvZmZzZXQiOjEwMH0",
  "total_estimate": 1240
}
```

`limit` defaults to 25 (max 100). Cursors are opaque and stable across inserts.

---

# 7. Filtering, Sorting, Field Selection

```http
GET /api/v1/workflows?status=running&sort=-created_at&fields=id,status,created_at
```

- `filter` via typed query params (`status`, `created_after`, `tag`, …).
- `sort` by field; `-` prefix for descending.
- `fields` for sparse responses (bandwidth control).

---

# 8. Error Model

A single, stable error shape across all endpoints:

```json
{
  "error": {
    "code": "validation_failed",
    "message": "field 'name' is required",
    "type": "client_error",
    "status": 400,
    "request_id": "req_01H...",
    "details": [ { "field": "name", "issue": "required" } ]
  }
}
```

| Status | `type` | Meaning |
|--------|--------|---------|
| 400 | client_error | Malformed/invalid request |
| 401 | auth_error | Unauthenticated |
| 403 | auth_error | Forbidden (policy/RBAC) |
| 404 | client_error | Not found |
| 409 | conflict | Version conflict / duplicate |
| 422 | client_error | Semantically invalid |
| 429 | rate_limit | Throttled |
| 5xx | server_error | Internal failure |

Subsystem-specific codes (e.g. `budget_exceeded`,
[`resource_exceeded`](../07-tool-runtime/execution-api.md#10-error-model)) reuse
this envelope.

---

# 9. Idempotency

Mutating requests accept an `Idempotency-Key` header:

```http
POST /api/v1/workflows:run
Idempotency-Key: run-invoice-2026-06-27
```

The Gateway dedupes retries within the key's TTL and returns the original result,
making client retries safe.

---

# 10. Concurrency Control

- Reads return an `ETag` (the resource `version`).
- Updates may send `If-Match: <version>`; a mismatch returns `409 conflict`.
- This prevents lost updates under concurrent edits.

---

# 11. Asynchronous Operations

Long-running actions return an **operation** the client polls or streams:

```json
{ "operation_id": "op_01H...", "status": "running", "resource": "wf_01H..." }
```

```http
GET  /api/v1/operations/{id}
GET  /api/v1/operations/{id}/stream      # SSE progress
```

Used by workflow runs, plugin installs, and bulk jobs.

---

# 12. Rate Limiting

- Limits apply per principal, API key, project, and tenant.
- Responses include `RateLimit-Limit`, `RateLimit-Remaining`, `RateLimit-Reset`.
- `429` responses carry `Retry-After`. Limits compose with subsystem quotas
  (e.g. [LLM Gateway](../05-llm-gateway/token-management.md#7-quotas-rolling)).

---

# 13. Authentication Summary

Every request authenticates via OAuth2/JWT, API key, or mTLS, and is authorized by
RBAC scopes and the [Policy Engine](../04-agent-framework/policy-engine.md). Full
detail in [Authentication](authentication.md).

---

# 14. Observability

- Each response carries a `request_id`; clients should log it.
- Requests are traced end to end (OpenTelemetry) and metered.
- Mutations emit domain events to the
  [Event Bus](../02-architecture/event-driven-architecture.md).

---

# 15. Webhooks & Events

Clients may register webhooks for resource events
(`workflow.completed`, `agent.run.failed`, `plugin.installed`, …). Deliveries are
signed, retried with backoff, and mirror Event Bus topics.

---

# 16. Dependencies

- [`02-architecture/c4-container.md`](../02-architecture/c4-container.md)
- [`09-api/authentication.md`](authentication.md)
- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md)

---

# 17. Related Documents

- [`09-api/index.md`](index.md)
- All resource documents in this section.

---

# 18. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.4.0 | 2026-07-14 | RM-AIM-P3 SRV-303: `GET /openapi.json` is now the generated, drift-proof ground-truth contract (derived from `#[utoipa::path]`/`ToSchema` on the handlers), and the CI contract gate lints that live document instead of the checked-in `openapi.yaml`, which remains only as a browsable snapshot |
| 1.3.0 | 2026-07-07 | Corrected the top divergence note: it said "no OAuth2/JWT" for the real API, which stopped being true once RM-GA-P1 SEC-101 shipped real JWT/API-key bearer verification (`APEX_AUTH_MODE`). Also noted `Idempotency-Key`'s RM-GA-P4 API-703 broadening to every mutating route. No API behavior changed — this was a stale-documentation fix found during a project-wide status review |
| 1.2.0 | 2026-07-04 | Linked the new [deprecation-policy.md](deprecation-policy.md) from §3; noted the TypeScript SDK's new retry/backoff and `paginateAll()` helper |
| 1.1.0 | 2026-07-03 | Added `openapi.yaml` as the hand-authored, ground-truth machine-readable contract (v1.0 "Stability" workstream), noting where this convention doc describes target-state behavior the real API doesn't implement (opaque ids, OAuth2/JWT, `/operations/{id}`). First TypeScript client (`sdks/typescript`) landed against the spec, integration-tested against a live `apex dev` server |
| 1.0.0 | 2026-06-27 | Initial Platform API Overview & Conventions |
