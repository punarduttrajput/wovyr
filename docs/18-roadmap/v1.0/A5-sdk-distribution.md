<!--
File: docs/18-roadmap/v1.0/A5-sdk-distribution.md
Document ID: GA-005
-->

# GA Completion: SDK Distribution & Migration Guides

**Document ID:** GA-005
**File Path:** `docs/18-roadmap/v1.0/A5-sdk-distribution.md`
**Version:** 1.0.0
**Status:** In progress — Python SDK published; TypeScript publish + more clients remain
**Owner:** Developer Experience Team
**Last Updated:** 2026-07-05

---

# 1. Purpose

Turn the "DX: SDK Distribution & Migration Guides" GA gap
([PRD-002 §5.5](../../01-product/prd-future.md#55-dx-sdk-distribution--migration-guides),
[v1.0 §3 DX row](../v1.0.md#3-in-scope)) into a delivery plan.

Committed GA-completion work — mostly done; the remaining items are distribution
and future-facing guides.

---

# 2. Current State

- **A hand-authored OpenAPI 3.0 contract** ([openapi.yaml](../../09-api/openapi.yaml))
  covers every route the server actually implements.
- **TypeScript SDK** (`sdks/typescript`, `@wovyr/sdk`) — full resource
  coverage, SSE parsing, `GET`-only retry/backoff, `paginateAll()`, a
  `redocly lint` contract check wired into `npm test`. **Built, packed, and
  verified importable from a real tarball — but not published to npm.**
- **Python SDK** (`sdks/python`, `wovyr-sdk`) — stdlib-only, mirrors the TS
  resource shape 1:1, `unittest`-tested against a live server, and **published to
  PyPI + verified installable.**
- **A deprecation-window policy** ([deprecation-policy.md](../../09-api/deprecation-policy.md))
  — 90-day minimum, `Deprecation`/`Sunset` headers — exists as a **process
  commitment, not enforced in code** (nothing has been deprecated yet).

---

# 3. Gap

1. The **TypeScript SDK is unpublished** — blocked on a live npm 2FA OTP the
   operator must supply interactively. This is a distribution blocker, **not a
   code gap**.
2. **No further language clients** (e.g. Go/Java) against the same contract.
3. **No migration guides**, and the deprecation-policy headers are **not enforced
   in code** — because nothing in `/api/v1` has been deprecated to write against
   yet.

---

# 4. Scope & Requirements

## 4.1 Functional / deliverables
- **Publish `@wovyr/sdk` to npm** (operator-supplied OTP). The package already
  builds/packs/imports correctly from a real tarball.
- **Evaluate additional language clients** (Go/Java) generated/hand-written
  against the same [openapi.yaml](../../09-api/openapi.yaml) contract.
- **Enforce the deprecation-policy headers in code** once there is a first
  `/v1`→`/v2` deprecation, and **author migration guides** at that point.

## 4.2 Non-functional
- New clients mirror the existing resource shape and error/pagination/retry
  semantics, keeping SDK parity across languages.
- The OpenAPI contract remains the single source both clients are checked against.

---

# 5. Exit Criteria

> `npm i @wovyr/sdk` installs a working client; SDK parity holds across shipped
> languages; and the deprecation-policy headers are enforced in code **once there
> is something to deprecate** (with a migration guide authored at that time).

Supports the v1.0 exit criterion of stable, documented SDKs
([v1.0 §5](../v1.0.md#5-exit-criteria)).

---

# 6. Dependencies & Environment Caveats

- **npm publish needs a live 2FA OTP** from the operator — an interactive step
  that doesn't fit a non-interactive session; deferred, not a code gap.
- **Migration guides are contingent** on an actual first deprecation occurring —
  there is nothing to migrate *from* yet, so writing one now would be fiction.

---

# 7. Risks

| Risk | Mitigation |
|------|-----------|
| SDK drift across languages | All clients checked against the one OpenAPI contract; mirror resource shape 1:1 |
| Deprecation headers claimed but unenforced | Explicitly scoped as enforced *when applicable*; policy doc is honest it's process-only today |
| npm publish repeatedly deferred | Track as a discrete operator action; the package is otherwise publish-ready |

---

# 8. Related Documents

- [`01-product/prd-future.md`](../../01-product/prd-future.md) §5.5 — requirements
- [`09-api/openapi.yaml`](../../09-api/openapi.yaml) — the contract
- [`09-api/deprecation-policy.md`](../../09-api/deprecation-policy.md)
- [`18-roadmap/v1.0.md`](../v1.0.md) — DX row

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-05 | Initial GA-completion delivery doc for SDK distribution & migration guides; records the published Python SDK + publish-ready TS SDK and scopes the remaining distribution/guide work |
