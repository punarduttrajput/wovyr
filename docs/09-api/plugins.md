<!--
File: docs/09-api/plugins.md
Document ID: API-007
-->

# Plugins API

**Document ID:** API-007  
**File Path:** `docs/09-api/plugins.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines the API for **managing plugins** — installing, enabling, upgrading, granting permissions, and browsing the marketplace. It is the control-plane interface to the [Plugin Engine](../08-plugin-sdk/overview.md).

All endpoints inherit the [API conventions](overview.md) and require
[authentication](authentication.md).

---

# 2. Resources

| Resource | Description |
|----------|-------------|
| `plugin` | An installed plugin (with versions + capabilities) |
| `grant` | A permission grant for a plugin in a scope |
| `listing` | A marketplace listing (read-only discovery) |

---

# 3. Endpoints

| Method | Path | Scope |
|--------|------|-------|
| GET | `/api/v1/plugins` | `plugins:read` |
| GET | `/api/v1/plugins/{id}` | `plugins:read` |
| POST | `/api/v1/plugins:install` | `plugins:admin` |
| POST | `/api/v1/plugins/{id}:enable` | `plugins:admin` |
| POST | `/api/v1/plugins/{id}:disable` | `plugins:admin` |
| POST | `/api/v1/plugins/{id}:upgrade` | `plugins:admin` |
| POST | `/api/v1/plugins/{id}:rollback` | `plugins:admin` |
| DELETE | `/api/v1/plugins/{id}` | `plugins:admin` |
| GET | `/api/v1/plugins/{id}/grants` | `plugins:read` |
| POST | `/api/v1/plugins/{id}/grants` | `plugins:admin` |
| DELETE | `/api/v1/plugins/{id}/grants/{gid}` | `plugins:admin` |
| GET | `/api/v1/marketplace/listings` | `plugins:read` |

---

# 4. Plugin Resource

```json
{
  "id": "plg_01H...",
  "object": "plugin",
  "name": "acme/github",
  "installed_version": "1.4.0",
  "channel": "stable",
  "capabilities": [
    { "kind": "tool", "id": "github.create_issue", "status": "enabled" },
    { "kind": "workflow_activity", "id": "github.wait_for_pr", "status": "enabled" }
  ],
  "permissions_requested": ["net:egress:api.github.com", "secret:read:github-token"],
  "trust": "verified",
  "status": "enabled"
}
```

Capability kinds and trust class follow the
[Plugin SDK overview](../08-plugin-sdk/overview.md#3-capability-kinds).

---

# 5. Install

```http
POST /api/v1/plugins:install
{ "name": "acme/github", "version": "1.4.0", "channel": "stable" }
```

Returns an [operation](overview.md#11-asynchronous-operations); the Plugin Engine
verifies signature/provenance, resolves dependencies, and checks compatibility
(see [Distribution §7](../08-plugin-sdk/distribution.md#7-install--pull-flow)).
Capabilities install **disabled** until granted and enabled.

---

# 6. Permission Grants

Plugins request permissions; grants authorize them per scope:

```http
POST /api/v1/plugins/plg_01H.../grants
{
  "project": "support-bot",
  "permissions": ["net:egress:api.github.com", "secret:read:github-token"]
}
```

The grant flow, scoping, and enforcement are specified in
[Plugin Permissions](../08-plugin-sdk/permissions.md). A `:upgrade` requesting new
permissions stages but does not enable the new capabilities until a fresh grant is
made.

---

# 7. Lifecycle Operations

| Action | Effect |
|--------|--------|
| `:enable` / `:disable` | Route capabilities in/out of their hosts (hot) |
| `:upgrade` | Install a new version, migrate, swap active (drains in-flight) |
| `:rollback` | Revert to the prior version |
| `DELETE` | Uninstall (blocked if other plugins depend on it) |

Semantics match [Plugin Versioning §7](../08-plugin-sdk/versioning.md#7-lifecycle-operations).

---

# 8. Marketplace Discovery

```http
GET /api/v1/marketplace/listings?category=scm&verified=true
```

Returns marketplace [listings](../08-plugin-sdk/marketplace.md#3-listing-model)
filtered by the deployment's
[marketplace policy](../08-plugin-sdk/marketplace.md#7-governance--curation)
(allowed publishers, required verification, permission-risk ceiling).

---

# 9. Governance

- Install/upgrade verify signature, provenance, and SBOM (fail-closed).
- Revoked versions are force-disabled
  ([Distribution §8](../08-plugin-sdk/distribution.md#8-revocation)).
- All lifecycle actions and grants are audited.

---

# 10. Events

Emits `plugin.installed`, `plugin.enabled`, `plugin.disabled`, `plugin.upgraded`,
and `plugin.permission.*` to the
[Event Bus](../02-architecture/event-driven-architecture.md).

---

# 11. Errors

Uses the [standard error envelope](overview.md#8-error-model). Notable codes:
`verification_failed`, `incompatible_version`, `unsatisfiable_dependencies`,
`forbidden` (grant required), `conflict` (dependency in use).

---

# 12. Dependencies

- [`08-plugin-sdk/overview.md`](../08-plugin-sdk/overview.md)
- [`08-plugin-sdk/permissions.md`](../08-plugin-sdk/permissions.md)
- [`08-plugin-sdk/versioning.md`](../08-plugin-sdk/versioning.md)
- [`08-plugin-sdk/distribution.md`](../08-plugin-sdk/distribution.md)

---

# 13. Related Documents

- [`09-api/tools.md`](tools.md)
- [`09-api/overview.md`](overview.md)

---

# 14. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Plugins API specification |
