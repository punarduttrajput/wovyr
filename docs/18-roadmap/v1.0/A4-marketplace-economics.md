<!--
File: docs/18-roadmap/v1.0/A4-marketplace-economics.md
Document ID: GA-004
-->

# GA Completion: Marketplace Economics & Safety

**Document ID:** GA-004
**File Path:** `docs/18-roadmap/v1.0/A4-marketplace-economics.md`
**Version:** 1.1.0
**Status:** In progress — the abuse-report + takedown workflow (§4.1) has
shipped (`Registry::report_abuse`/`resolve_abuse_report`/`dismiss_abuse_report`,
with resolve-time delisting, over server routes + CLI commands, audited);
monetization and the dashboard browse UI remain open
**Owner:** Ecosystem Team
**Last Updated:** 2026-07-15

---

# 1. Purpose

Turn the "Ecosystem: Marketplace Economics & Safety" GA gap
([PRD-002 §5.4](../../01-product/prd-future.md#54-ecosystem-marketplace-economics--safety),
[v1.0 §3 Ecosystem row](../v1.0.md#3-in-scope)) into a delivery plan.

Committed GA-completion work. This covers the **economic engine and safety** of
the first-party marketplace — distinct from the exploratory *federation/interop*
frontier in [FUT-005](../future/B5-ecosystem-interop.md).

---

# 2. Current State

- **The marketplace core is done** (v0.3): a `Registry` control plane over a
  signed package supply chain — publish (re-verifies ed25519 signatures against a
  shared `TrustStore`), operator curation (`RegistryPolicy`), discovery/search,
  download, reviews, install counts, automated static security scanning
  (`scan.rs`), and the full human-review/verified-badge workflow.
- **Multi-node ready.** A capability-gated `PostgresRegistryStore` lets a fleet
  share one durable catalog; both server and CLI select it at runtime.
- **Abuse handling is now shipped.** Any principal can file an `AbuseReport`
  against a listing; a moderator resolves (optionally **delisting** — hidden from
  search/get, download refused, like a policy blocklist entry but as a dynamic
  moderation decision) or dismisses it — over server routes
  (`.../report`, `.../reports`, `.../reports/{id}/resolve|dismiss`) and the
  matching `apex plugin report|reports|resolve-abuse|dismiss-abuse` CLI commands,
  emitting `plugin.abuse_reported`/`plugin.delisted`.
- **No economics, no browse UI.** There is still no monetization or revenue
  share, and the dashboard SPA covers every core surface *except* a marketplace
  browse experience.

---

# 3. Gap

The marketplace can publish, govern, install, and now police plugins (the abuse
workflow is its safety valve), but there is still **no incentive loop**
(monetization) to grow a third-party ecosystem, and **no consumer-facing browse
UI**.

---

# 4. Scope & Requirements

## 4.1 Functional / deliverables
- A **monetization model** (paid listings / revenue share) with billing
  integration behind a **provider-neutral trait** (no lock-in to one billing
  vendor), consistent with the platform's abstraction discipline.
- An **abuse-report + takedown workflow**, paralleling the existing human-review
  workflow (`request_review`/`approve`/`reject` → an analogous
  report/triage/disable path).
- A **marketplace browse/search UI** in the dashboard SPA, over the existing
  `search`/listing/download registry surface.

## 4.2 Non-functional
- Billing is behind a trait so the platform stays provider-neutral.
- Abuse actions are audited (like other governance actions) and reversible where
  appropriate.

---

# 5. Exit Criteria

> A **paid plugin** can be listed, purchased, installed, and its publisher paid
> end to end; a **reported plugin** can be triaged and disabled through the abuse
> workflow; and the browse UI surfaces listings from the live registry.

---

# 6. Dependencies & Environment Caveats

- Monetization needs a **billing provider integration** (real vendor) — the trait
  boundary can be built and tested with a mock here, but a live transaction needs
  a real account.
- The browse UI builds on the existing dashboard SPA and registry search — no new
  backend surface required.
- **Precedes** the exploratory federation work
  ([FUT-005](../future/B5-ecosystem-interop.md)): a healthy first-party economic
  ecosystem should exist before cross-org federation is attempted.

---

# 7. Risks

| Risk | Mitigation |
|------|-----------|
| Billing-vendor lock-in | Provider-neutral billing trait, mirroring other platform abstractions |
| Abuse workflow abused (bad-faith reports) | Triage step with reviewer authority, mirroring the human-review model |
| Monetization outpacing safety | Ship abuse handling alongside (not after) monetization |

---

# 8. Related Documents

- [`01-product/prd-future.md`](../../01-product/prd-future.md) §5.4 — requirements
- [`08-plugin-sdk/marketplace.md`](../../08-plugin-sdk/marketplace.md)
- [`18-roadmap/future/B5-ecosystem-interop.md`](../future/B5-ecosystem-interop.md) — the interop/federation frontier (Tier B)
- [`18-roadmap/v1.0.md`](../v1.0.md) — Ecosystem row

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.1.0 | 2026-07-15 | Status Planned → In progress: the abuse-report/takedown workflow shipped (registry + server routes + CLI, with delisting + audit events); Current State/Gap updated to match. Monetization and the browse UI remain open |
| 1.0.0 | 2026-07-05 | Initial GA-completion delivery doc for marketplace economics & safety (monetization, abuse workflow, browse UI) |
