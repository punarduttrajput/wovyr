<!--
File: docs/10-dashboard/marketplace.md
Document ID: DASH-005
-->

# Marketplace UI

**Document ID:** DASH-005  
**File Path:** `docs/10-dashboard/marketplace.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document specifies the **Marketplace UI** — where users discover, evaluate, install, and manage plugins. It is the visual front end over the [Plugins API](../09-api/plugins.md) and the [Plugin Marketplace](../08-plugin-sdk/marketplace.md).

---

# 2. Surfaces

| View | Purpose |
|------|---------|
| Browse | Search/filter [listings](../08-plugin-sdk/marketplace.md#3-listing-model) |
| Listing detail | Capabilities, permissions, versions, ratings, provenance |
| Install | Install + grant flow |
| Installed | Manage installed plugins (enable/disable/upgrade/rollback) |
| Grants | Review and modify permission grants |

---

# 3. Discovery

The browse view queries
[`/marketplace/listings`](../09-api/plugins.md#8-marketplace-discovery) with search,
category, and capability filters. Results respect the deployment's
[marketplace policy](../08-plugin-sdk/marketplace.md#7-governance--curation) — users
only see plugins they are permitted to install.

[Trust signals](../08-plugin-sdk/marketplace.md#5-trust-signals) (verified badges,
ratings, install counts, declared permissions) are shown prominently.

---

# 4. Listing Detail

For a plugin, the detail page shows:

- Capabilities by [kind](../08-plugin-sdk/overview.md#3-capability-kinds) (tools, providers, …)
- **Requested permissions**, grouped by risk, before install
- Version history and channels (stable/beta/edge)
- Provenance/SBOM availability
- Ratings, reviews, publisher info

---

# 5. Install & Consent Flow

```text
Choose version/channel
   │
   ▼
Review requested permissions (risk-highlighted)
   │
   ▼
Confirm install → operation progress (verify → resolve → register)
   │
   ▼
Grant permissions (per project) → enable capabilities
```

This UI realizes the [permission consent UX](../08-plugin-sdk/permissions.md#8-consent-ux):
broad/wildcard permissions are flagged, and a subset can be granted. Install runs as
an [operation](../09-api/overview.md#11-asynchronous-operations) with live status.

---

# 6. Managing Installed Plugins

| Action | API |
|--------|-----|
| Enable / Disable | [`:enable` / `:disable`](../09-api/plugins.md#7-lifecycle-operations) |
| Upgrade / Rollback | [`:upgrade` / `:rollback`](../09-api/plugins.md#7-lifecycle-operations) |
| Edit grants | [grants endpoints](../09-api/plugins.md#6-permission-grants) |
| Uninstall | [`DELETE`](../09-api/plugins.md#3-endpoints) |

An **upgrade requesting new permissions** shows a permission diff and requires a
fresh grant before the new capabilities go live
([versioning](../08-plugin-sdk/versioning.md#8-upgrade-safety)).

---

# 7. Governance Views (Operators)

Operators can:

- Configure the [marketplace policy](../08-plugin-sdk/marketplace.md#7-governance--curation)
  (allowed publishers, require-verified, permission-risk ceiling, blocklist).
- See which projects have which plugins/grants.
- Respond to [revocations](../08-plugin-sdk/distribution.md#8-revocation) (force-disabled versions are flagged).

---

# 8. Publishing (Publishers)

Verified publishers can manage their listings, channels, and respond to reviews
from within the dashboard, backed by the
[publish flow](../08-plugin-sdk/distribution.md#6-publish-flow).

---

# 9. Governance

- Install/upgrade/grant require `plugins:admin`; browsing requires `plugins:read`.
- All actions are audited; permission grants are explicit and revocable.

---

# 10. Dependencies

- [`09-api/plugins.md`](../09-api/plugins.md)
- [`08-plugin-sdk/marketplace.md`](../08-plugin-sdk/marketplace.md)
- [`08-plugin-sdk/permissions.md`](../08-plugin-sdk/permissions.md)

---

# 11. Related Documents

- [`10-dashboard/overview.md`](overview.md)
- [`10-dashboard/settings.md`](settings.md)

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Marketplace UI specification |
