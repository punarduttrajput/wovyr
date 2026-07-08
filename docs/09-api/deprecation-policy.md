<!--
File: docs/09-api/deprecation-policy.md
Document ID: API-002
-->

# API Deprecation & Compatibility Window Policy

**Document ID:** API-002
**File Path:** `docs/09-api/deprecation-policy.md`
**Version:** 1.1.0
**Status:** Active policy, **now mechanically enforceable** (RM-GA-P4
API-705): `crates/apex-server/src/hardening.rs`'s `DEPRECATIONS` table +
`deprecation_headers` middleware emit the `Deprecation`/`Sunset` headers §4
describes for any route added to the table. Still nothing in `/api/v1` has
been deprecated, so the table is empty and no endpoint carries the headers
today — this closes the "policy exists in prose only" gap, it doesn't create
a first deprecation.
**Owner:** AI Platform Team
**Last Updated:** 2026-07-08

---

# 1. Purpose

Defines how long a client can rely on a given `/api/v1` behavior before it may
change or disappear, and what the platform commits to publish before that
happens. This is the concrete policy [overview.md §3](overview.md#3-base-url--versioning)
gestures at ("deprecations announced with a window") without specifying a
duration or mechanism.

---

# 2. Scope

Applies to the REST surface documented in [`openapi.yaml`](openapi.yaml): every
`/api/v1/*` route, its request/response schema, and the semantics of its
headers (`X-Apex-Tenant`, `Idempotency-Key`, `If-Match`, …). It does **not**
cover:

- Internal crate APIs (`apex-*` Rust crates) — those follow normal semver via
  `Cargo.toml`, not this policy.
- Undocumented behavior — if it's not in `openapi.yaml`, relying on it is at
  the caller's own risk (e.g. incidental error-message wording).
- Pre-GA (`v1.0` not yet tagged) breaking changes — until the platform reaches
  GA, `/api/v1` may still change without a deprecation window. This policy
  takes full effect at the v1.0 tag.

---

# 3. What Counts as a Breaking Change

Not breaking (ships without a deprecation window, per [overview.md §3](overview.md#3-base-url--versioning)):

- New endpoints, new optional request fields, new response fields.
- New enum members in a response field, **provided** clients are expected to
  treat unrecognized values as opaque (documented per-field where this
  applies, e.g. `WorkflowStatus`).
- New optional headers.

Breaking (requires this policy's window before removal/change):

- Removing or renaming an endpoint, field, header, or query parameter.
- Changing a field's type, or a previously-optional field becoming required.
- Changing a status code's meaning for an existing request shape.
- Tightening validation that previously-accepted requests would now fail.
- Changing default behavior a client could reasonably have depended on (e.g.
  the `limit` default in [overview.md §6](overview.md#6-pagination)).

---

# 4. The Window

| Stage | Requirement |
|-------|-------------|
| **Announce** | The change is documented (this repo's changelog + the affected resource doc) and, from that point, every affected response carries a `Deprecation: true` header and a `Sunset: <RFC 7231 date>` header ([RFC 8594](https://www.rfc-editor.org/rfc/rfc8594)) naming the date below. |
| **Minimum window** | **90 days** from the `Deprecation` header first appearing in production to the earliest date the old behavior may be removed. Security-driven deprecations (e.g. an auth downgrade) may ship with a **shorter, explicitly justified** window — never zero, and never without the headers. |
| **Migration path** | The announcement links a migration note: what changes, why, and the replacement call/field. For a removed endpoint, the replacement (or explicit "no replacement, here's why") must exist *before* the sunset date, not at it. |
| **Removal** | On or after the sunset date, the old behavior may be removed in a routine release — no additional notice required, since the window already served that purpose. |

A breaking change that cannot fit this window (e.g. an urgent security fix
with no safe deprecation path) requires an explicit, documented exception
signed off by the API owner — this is the escape hatch, not the default.

---

# 5. Major Version Bumps

A `/api/v2` is for changes too broad to express as a series of individual
deprecations (e.g. a new auth model). Per [overview.md §3](overview.md#3-base-url--versioning):

- `/v1` and `/v2` run **in parallel** for the duration of `/v1`'s remaining
  deprecation windows — `/v2` shipping does not shorten any `/v1` window
  already in flight.
- `/v1` as a whole is announced for removal the same way an individual
  endpoint is (§4), with the same 90-day floor, once `/v2` reaches parity.

---

# 6. Plugin API

The plugin-facing surface ([`apex-plugin`](../08-plugin-sdk/overview.md)'s
`PlatformApi` compatibility range a manifest declares) follows the same
window, with one addition: `PluginEngine::upgrade` already refuses an upgrade
that would break an installed **dependent's** version requirement (see
`crates/apex-plugin`), so a plugin author changing their own capability
surface inherits this policy for their consumers the same way the platform
does for API clients.

---

# 7. Current State

No `/api/v1` endpoint has been deprecated. **The enforcement mechanism now
exists (RM-GA-P4 API-705)**: `hardening::DEPRECATIONS` is a `const` table of
`(method, path, deprecated_since, sunset)` entries — empty today — and
`hardening::deprecation_headers` (wired into every route in `router()`)
stamps `Deprecation: true` and an RFC 7231 `Sunset` date on any response
matching a table entry. Adding a real deprecation is a one-line table entry,
no other code change. A test (`deprecation_table_windows_are_valid`) fails
CI if a future entry's window is under the §4 90-day minimum. The TypeScript
SDK ([`sdks/typescript`](../../sdks/typescript)) still does not inspect
`Deprecation`/`Sunset` response headers or surface them to callers — that
remains the first piece of *client-side* tooling due when this policy is
exercised for real.

---

# 8. Related

- [`09-api/overview.md`](overview.md) §3, §18
- [`09-api/openapi.yaml`](openapi.yaml)
- [`18-roadmap/v1.0.md`](../18-roadmap/v1.0.md)

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.1.0 | 2026-07-08 | API-705 done: added the `hardening::DEPRECATIONS` table + `deprecation_headers` middleware, making this policy mechanically enforceable rather than prose-only. Table is empty (no real deprecation exists); a test enforces the 90-day window on any future entry |
| 1.0.0 | 2026-07-04 | Initial deprecation-window policy: 90-day minimum, `Deprecation`/`Sunset` headers, breaking-change definition, `/v2` parallel-run rule |
