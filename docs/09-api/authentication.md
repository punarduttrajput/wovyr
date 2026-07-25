<!--
File: docs/09-api/authentication.md
Document ID: API-002
-->

# API Authentication & Authorization

**Document ID:** API-002  
**File Path:** `docs/09-api/authentication.md`  
**Version:** 1.1.0  
**Status:** Draft — this document describes the **target-state** design (full
OAuth2/OIDC flows, refresh tokens, per-key IP allowlists, mTLS, org-custom
roles). **What `wovyr-server` actually implements today** (RM-GA-P1 SEC-101,
added 2026-07-07): a bearer credential verified by an `auth::authenticate`
middleware before any handler runs, selected via `WOVYR_AUTH_MODE` —
`jwt` (HS256 via `WOVYR_JWT_HS_SECRET` or RS256 via `WOVYR_JWT_RS_PUBLIC_KEY`,
with optional issuer/audience checks; the verified `sub` claim becomes the
principal) or `apikey` (a bearer token SHA-256-hashed and looked up in a
file-backed store, minted via `wovyr auth create-key`). **API keys now have a
full lifecycle** (RM-AIM-P1 SRV-104, 2026-07-14): create-with-TTL, list,
**revoke**, and rotate-with-grace (`wovyr auth create-key --ttl-days |
list-keys | revoke <id> | rotate <id> --grace-hours`), with revocation and
expiry enforced on every lookup and `last_used` tracking. Still not
implemented: OAuth2 authorization flows, refresh tokens, mTLS, per-key IP
allowlists, and any revocation mechanism for **JWTs** (a JWT stays valid to
expiry) — a verified credential simply overwrites the request's
`X-Wovyr-Principal` header before RBAC runs. See
`crates/wovyr-server/src/auth.rs` and
[`phase1-security-floor-tickets.md`](../18-roadmap/v1.0/phase1-security-floor-tickets.md).  
**Owner:** AI Platform Team  
**Last Updated:** 2026-07-15

---

# 1. Purpose

This document defines how API callers **authenticate** (prove identity) and how requests are **authorized** (granted access). It covers credential types, token formats, RBAC scopes, and policy enforcement at the API Gateway.

Authentication establishes *who*; authorization decides *what they may do*; the
[Policy Engine](../04-agent-framework/policy-engine.md) enforces contextual rules.

---

# 2. Credential Types

| Credential | Use | Carried as |
|------------|-----|-----------|
| OAuth2 / OIDC access token (JWT) | Interactive users, SSO | `Authorization: Bearer <jwt>` |
| API key | Services, CI, CLI | `Authorization: Bearer <key>` or `Wovyr-Api-Key` |
| Service token (short-lived JWT) | Internal service-to-service | `Authorization: Bearer <jwt>` |
| mTLS client cert | Internal, zero-trust networks | TLS handshake |

All external traffic is TLS; internal service traffic uses mTLS.

---

# 3. OAuth2 / OIDC

The platform supports standard OAuth2 flows via an external or built-in IdP:

| Flow | Use |
|------|-----|
| Authorization Code + PKCE | Web/desktop user login |
| Client Credentials | Machine-to-machine |
| Device Code | CLI / headless |
| Refresh Token | Session renewal |

Access tokens are JWTs validated at the Gateway (signature, `exp`, `aud`, `iss`).
SSO via OIDC lets enterprises bring their own IdP.

---

# 4. JWT Claims

```json
{
  "sub": "user_01H...",
  "tenant": "acme",
  "org": "acme-eu",
  "projects": ["support-bot"],
  "roles": ["workflow.editor", "agent.operator"],
  "scopes": ["agents:read", "agents:run", "workflows:write"],
  "exp": 1750003600,
  "iss": "https://auth.wovyr.example.com",
  "aud": "wovyr-api"
}
```

The token binds the principal to a tenant, roles, and scopes; the Gateway derives
the effective permission set from these.

---

# 5. API Keys

- Created per project or service account; see [Users API §6](users.md#6-api-keys).
- Prefixed and partially shown once (`apx_live_…`), stored hashed.
- Carry a fixed scope set and optional IP allowlist and expiry.
- Revocable instantly; usage is audited.

---

# 6. Authorization Model (RBAC + ABAC)

```text
Authenticated principal
   │
   ▼
RBAC: roles → scopes        (coarse: may call this endpoint?)
   │
   ▼
Resource scoping            (tenant/project/ownership match?)
   │
   ▼
ABAC via Policy Engine      (contextual: data class, region, time, risk)
   │
   ├── allow → proceed
   └── deny  → 403 + audit
```

RBAC gates *which operations*; resource scoping ensures the principal acts within
its tenant/project; ABAC applies fine-grained, attribute-based rules. Enforcement
is **fail-closed**.

---

# 7. Scopes

Scopes follow `resource:action`:

```text
agents:read     agents:write     agents:run
workflows:read  workflows:write  workflows:run  workflows:cancel
memory:read     memory:write
tools:read      tools:invoke
plugins:read    plugins:admin
projects:admin  users:admin
```

A token's effective scopes are the union granted by its roles, intersected with
any API-key scope restriction.

---

# 8. Roles

Roles bundle scopes; built-in roles include:

| Role | Grants |
|------|--------|
| `viewer` | `*:read` |
| `operator` | reads + `*:run` |
| `editor` | reads + writes |
| `project.admin` | full within a project |
| `org.admin` | full within an organization |
| `platform.admin` | full across the deployment |

Custom roles can be defined per organization (see [Users API](users.md)).

---

# 9. Tenant & Project Scoping

- Every request resolves to exactly one tenant; cross-tenant access is impossible.
- Project-scoped tokens are confined to their project(s).
- Resource ownership is checked on every read/write so principals see only
  authorized resources (consistent with
  [Memory scopes](../06-memory-engine/memory-api.md#10-scopes--sharing)).

---

# 10. Sessions & Token Lifecycle

| Concern | Behavior |
|---------|----------|
| Access token TTL | Short (minutes) |
| Refresh token | Longer; rotated on use |
| Revocation | Token blocklist + key revocation, effective immediately |
| Rotation | API keys and signing keys rotate on schedule |

---

# 11. Secrets & Credentials

API credentials and provider secrets are stored in the secret vault as references,
never returned in responses, and rotated per policy (aligned with
[Provider SDK §20](../04-agent-framework/provider-sdk.md#20-security) and
[Tool Runtime secrets](../07-tool-runtime/security-isolation.md#7-secret-management)).

---

# 12. Audit

Every authentication and authorization decision is audited:

```json
{
  "event": "api.authz.denied",
  "principal": "user_01H...",
  "tenant": "acme",
  "endpoint": "POST /api/v1/workflows:run",
  "scope_required": "workflows:run",
  "reason": "missing_scope",
  "request_id": "req_01H...",
  "timestamp": "2026-06-27T10:00:00Z"
}
```

---

# 13. Errors

| Code | Status | Meaning |
|------|--------|---------|
| `unauthenticated` | 401 | Missing/invalid/expired credential |
| `forbidden` | 403 | Authenticated but not authorized |
| `token_expired` | 401 | Access token expired (refresh) |
| `key_revoked` | 401 | API key revoked |

These use the standard [error envelope](overview.md#8-error-model).

---

# 14. Dependencies

- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md)
- [`09-api/users.md`](users.md)
- [`13-security`](../SUMMARY.md) *(planned: platform security)*

---

# 15. Related Documents

- [`09-api/overview.md`](overview.md)
- [`09-api/projects.md`](projects.md)

---

# 16. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.2.0 | 2026-07-15 | Status note refreshed for RM-AIM-P1 SRV-104: API keys now have TTL/revoke/rotate-with-grace lifecycle; clarified the remaining gap is JWT revocation + OAuth2/mTLS/IP allowlists. No design content changed |
| 1.1.0 | 2026-07-07 | Added a top note distinguishing this doc's target-state design from what's actually implemented (RM-GA-P1 SEC-101: JWT/API-key bearer auth, no OAuth2 flow/mTLS/refresh tokens yet). Found during a project-wide status review; no design content changed |
| 1.0.0 | 2026-06-27 | Initial API Authentication & Authorization specification |
