<!--
File: docs/09-api/users.md
Document ID: API-009
-->

# Users API

**Document ID:** API-009  
**File Path:** `docs/09-api/users.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines the API for managing **users, roles, teams, service accounts, and API keys** — the identities and credentials that act on the platform.

It is the identity-management counterpart to [Authentication](authentication.md),
which defines how those identities prove themselves and are authorized.

---

# 2. Resources

| Resource | Description |
|----------|-------------|
| `user` | A human identity |
| `service_account` | A non-human identity for automation |
| `role` | A named bundle of scopes |
| `team` | A group of users for bulk role assignment |
| `api_key` | A credential for a user or service account |

---

# 3. Endpoints

| Method | Path | Scope |
|--------|------|-------|
| GET | `/api/v1/users` | `users:admin` |
| POST | `/api/v1/users:invite` | `users:admin` |
| GET | `/api/v1/users/{id}` | `users:read` |
| PATCH | `/api/v1/users/{id}` | `users:admin` |
| DELETE | `/api/v1/users/{id}` | `users:admin` |
| GET | `/api/v1/users/me` | (any authenticated) |
| GET | `/api/v1/service-accounts` | `users:admin` |
| POST | `/api/v1/service-accounts` | `users:admin` |
| GET | `/api/v1/roles` | `users:read` |
| POST | `/api/v1/roles` | `org.admin` |
| GET | `/api/v1/teams` | `users:read` |
| POST | `/api/v1/teams` | `users:admin` |
| POST | `/api/v1/api-keys` | `users:admin` |
| DELETE | `/api/v1/api-keys/{id}` | `users:admin` |

---

# 4. User Resource

```json
{
  "id": "user_01H...",
  "object": "user",
  "email": "alex@example.com",
  "tenant": "acme",
  "status": "active",
  "identity_provider": "oidc:okta",
  "memberships": [
    { "project": "support-bot", "role": "editor" },
    { "organization": "org_01H...", "role": "viewer" }
  ]
}
```

`memberships` mirror [Projects API §6](projects.md#6-membership--roles). User
provisioning may be just-in-time via OIDC/SSO or via explicit invite.

---

# 5. Service Accounts

Non-human identities for automation, CI, and integrations:

```json
{
  "id": "svc_01H...",
  "object": "service_account",
  "name": "ci-deployer",
  "project": "support-bot",
  "roles": ["workflow.operator"]
}
```

Service accounts authenticate via API keys or the OAuth2 client-credentials flow
([Authentication §3](authentication.md#3-oauth2--oidc)). They cannot log in
interactively.

---

# 6. API Keys

```http
POST /api/v1/api-keys
{ "subject": "svc_01H...", "name": "deploy-key", "scopes": ["workflows:run"], "expires_at": "2027-01-01T00:00:00Z" }
```

Response (the secret is shown **once**):

```json
{ "id": "key_01H...", "secret": "apx_live_9f2c…", "prefix": "apx_live_9f2c", "scopes": ["workflows:run"] }
```

Keys are stored hashed, carry a fixed scope set (a subset of the subject's
permissions), support optional IP allowlists and expiry, and are revocable
instantly. See [Authentication §5](authentication.md#5-api-keys).

---

# 7. Roles & Custom Roles

Built-in roles are defined in [Authentication §8](authentication.md#8-roles).
Organizations may define custom roles bundling specific scopes:

```http
POST /api/v1/roles
{ "name": "incident-responder", "scopes": ["workflows:run", "memory:read", "tools:invoke"] }
```

A custom role's scopes are bounded by what the creating admin may delegate.

---

# 8. Teams

Teams assign roles to many users at once:

```json
{ "id": "team_01H...", "name": "support-engineers", "members": ["user_01H...", "user_02H..."] }
```

Assigning a role to a team grants it to all members; membership changes propagate
to effective permissions immediately.

---

# 9. Identity Lifecycle

| Event | Behavior |
|-------|----------|
| Invite | Email/SSO provisioning; pending until accepted |
| Deactivate | Sessions revoked; keys disabled; data retained |
| Delete | Soft delete; reassign owned resources per policy |
| SSO deprovision | SCIM/OIDC removal cascades to memberships |

---

# 10. Self-Service

`GET /api/v1/users/me` returns the caller's profile, memberships, and effective
scopes — used by the dashboard and CLI to tailor available actions.

---

# 11. Governance

- All identity and key operations require admin scopes and are audited.
- API key creation, rotation, and revocation emit events and audit records.
- Least-privilege is encouraged: keys/roles should grant the minimum needed.

---

# 12. Events

Emits `user.invited`, `user.deactivated`, `role.created`, `team.updated`,
`apikey.created`, `apikey.revoked` to the
[Event Bus](../02-architecture/event-driven-architecture.md).

---

# 13. Errors

Uses the [standard error envelope](overview.md#8-error-model). Notable codes:
`forbidden` (admin required), `conflict` (email/name exists),
`invalid_request` (scope exceeds delegator's).

---

# 14. Dependencies

- [`09-api/authentication.md`](authentication.md)
- [`09-api/projects.md`](projects.md)
- [`04-agent-framework/policy-engine.md`](../04-agent-framework/policy-engine.md)

---

# 15. Related Documents

- [`09-api/overview.md`](overview.md)
- [`13-security`](../SUMMARY.md) *(planned: platform security)*

---

# 16. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Users API specification |
