<!--
File: docs/08-plugin-sdk/marketplace.md
Document ID: PLG-007
-->

# Plugin Marketplace

**Document ID:** PLG-007  
**File Path:** `docs/08-plugin-sdk/marketplace.md`  
**Version:** 1.1.0  
**Status:** Core implemented — the `apex-marketplace` registry crate provides the listing
model, governance policy, ratings, and the publish → discover → download → install flow
(durable `File`/`InMemory` stores), surfaced over the server's `/api/v1/marketplace*`
routes and the `apex plugin publish|search|get` CLI. Deferred: automated security
scanning + the full review workflow (only the operator `verify` toggle exists),
recommendations, abuse-report workflow, and monetization (§9).  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-30

---

# 1. Purpose

This document defines the **Plugin Marketplace** — the discovery, publishing, quality, and governance surface for plugins. The marketplace is the ecosystem layer that realizes the platform's goal to *foster a vibrant plugin ecosystem* (see [Vision](../00-executive/vision.md)).

The marketplace builds on [Distribution](distribution.md) (the verified supply
chain) and adds people-facing concerns: finding plugins, trusting them, and
sustaining publishers.

---

# 2. Roles

| Role | Does |
|------|------|
| Consumer | Discovers, installs, rates plugins |
| Publisher | Builds, publishes, maintains plugins |
| Reviewer | Vets submissions for quality/security |
| Operator | Curates what a deployment may install |

---

# 3. Listing Model

A marketplace listing aggregates a plugin's published versions and metadata:

```yaml
listing:
  id: acme/github
  display_name: GitHub
  publisher: acme            # verified identity
  categories: [devtools, scm]
  capabilities: [tool, workflow_activity]
  versions: [1.4.0, 1.3.2, 1.3.1]
  channels: { stable: 1.4.0, beta: 1.5.0-rc1 }
  rating: 4.7
  installs: 12840
  verified: true
```

Listings show declared **permissions** and **capabilities** up front so consumers
see what they are granting before install (ties to
[Permissions §8](permissions.md#8-consent-ux)).

---

# 4. Discovery

| Mechanism | Description |
|-----------|-------------|
| Search | Full-text over name, description, capabilities |
| Categories / tags | Browse by domain (devtools, data, comms, …) |
| Capability filter | "providers that support vision", "tools needing no egress" |
| Curated collections | Editor/operator picks |
| Recommendations | Based on installed plugins and usage |

Discovery is available in the dashboard and via the CLI (`apex plugin search`).

---

# 5. Trust Signals

Consumers judge a plugin by:

- **Verified publisher** badge (identity-verified)
- **Verified plugin** badge (passed review)
- Ratings and reviews
- Install count and active-install trend
- Declared permissions (fewer/narrower = safer)
- Provenance/SBOM availability ([Distribution §4](distribution.md#4-provenance--sbom))
- Maintenance recency and deprecation status

Community (unreviewed) plugins are clearly labeled and default to the most
restrictive [trust class](overview.md#7-trust-model).

---

# 6. Review & Quality

Submissions to the public marketplace (especially `stable`) pass:

```text
Submit → automated checks → security scan → human review (for verified) → publish
```

Automated checks: manifest/schema validity, signature + provenance, compatibility,
permission sanity (no undeclared usage), and static security scanning. Verified
status additionally requires human review. Failing review blocks publish with
actionable feedback.

---

# 7. Governance & Curation

Operators control what their deployment exposes:

```yaml
marketplace_policy:
  sources: [public, private-acme]
  allow_publishers: [acme, apex-official]
  require_verified: true
  max_permission_risk: medium       # blocks broad/wildcard-permission plugins
  blocklist: [some/abandoned-plugin]
```

This lets enterprises offer a **curated internal catalog** drawn from the public
marketplace plus private plugins, enforcing their own risk bar.

---

# 8. Ratings & Feedback

- Ratings (1–5) and written reviews from verified installs only (reduces spam).
- Publishers can respond to reviews.
- Aggregate quality + abuse-report signals feed listing ranking and can trigger
  re-review or delisting.

---

# 9. Monetization (Planned)

The marketplace supports (future) commercial plugins:

| Model | Description |
|-------|-------------|
| Free / OSS | No charge |
| Paid | One-time or subscription license |
| Usage-based | Billed via platform metering (ties to cost events) |
| Revenue share | Platform/publisher split |

Billing reuses the platform's metering and cost-event pipeline
([LLM Gateway token management](../05-llm-gateway/token-management.md) pattern).
This is roadmap, not v1.

---

# 10. Lifecycle Integration

- Publishing emits `plugin.published`; the marketplace indexes the new version.
- Channels and deprecation mirror [Versioning §6/§9](versioning.md#6-channels).
- Revocation ([Distribution §8](distribution.md#8-revocation)) delists and warns
  affected installs.

---

# 11. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Search latency | < 200 ms p95 |
| Listing freshness after publish | < 60 s |
| Verified-review SLA | publisher-facing target |
| Abuse-report handling | tracked workflow |

---

# 12. Dependencies

- [`08-plugin-sdk/distribution.md`](distribution.md)
- [`08-plugin-sdk/versioning.md`](versioning.md)
- [`08-plugin-sdk/permissions.md`](permissions.md)
- [`02-architecture/domain-driven-design.md`](../02-architecture/domain-driven-design.md)

---

# 13. Related Documents

- [`08-plugin-sdk/overview.md`](overview.md)
- [`00-executive/vision.md`](../00-executive/vision.md)
- [`10-dashboard`](../SUMMARY.md) *(planned: marketplace UI)*

---

# 14. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Plugin Marketplace specification |
| 1.1.0 | 2026-06-30 | Core implemented: `apex-marketplace` crate (listing model, `RegistryPolicy` governance, ratings, signature-verified publish, discovery, download, install bridge; `File`/`InMemory` stores) + server `/api/v1/marketplace*` routes (publish/search/get/download/review/verify/install, emits `plugin.published`) + CLI `plugin publish|search|get` |
