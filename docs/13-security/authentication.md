<!--
File: docs/13-security/authentication.md
Document ID: SEC-001
-->

# Security: Authentication

**Document ID:** SEC-001  
**File Path:** `docs/13-security/authentication.md`  
**Version:** 1.1.0  
**Status:** Draft — target-state security model. **Current implementation**
(RM-GA-P1 SEC-101/SEC-102, added 2026-07-07): `crates/apex-server/src/auth.rs`
verifies a JWT (HS256/RS256) or hashed API-key bearer credential before any
handler runs, fail-closed by default — the `disabled-loopback` mode (no
verification, today's back-compat behavior) requires an explicit
`APEX_ALLOW_ANONYMOUS=1` opt-in that a startup check
(`auth::refuse_anonymous_on_non_loopback`) refuses to honor on any
non-loopback bind. **API-key lifecycle is now real** (RM-AIM-P1 SRV-104,
2026-07-14): create-with-TTL, list, revoke, and rotate-with-grace, with
revocation/expiry enforced on every lookup. Not yet implemented: OAuth2/OIDC
SSO, mTLS, MFA/step-up, browser session cookies, and any revocation for
**JWTs** (valid to expiry) — the rest of this document's design.  
**Owner:** Security Team  
**Last Updated:** 2026-07-15

---

# 1. Purpose

This document defines the platform-wide **authentication** model — how human and machine identities are established and verified before any authorization decision.

It is the security reference behind the API-facing
[API Authentication](../09-api/authentication.md); that document covers the
developer contract, this one the security guarantees.

---

# 2. Identity Types

| Identity | Authenticated by |
|----------|------------------|
| Human user | OAuth2/OIDC (SSO) |
| Service account | API key or OAuth2 client credentials |
| Internal service | mTLS + service identity |
| Plugin/tool | Derived, scoped identity (no standalone credentials) |

---

# 3. Credential Mechanisms

- **OAuth2 / OIDC** — Authorization Code + PKCE (web), Device Code (CLI), Client
  Credentials (M2M). Tokens are JWTs validated for signature, `exp`, `aud`, `iss`.
- **API keys** — hashed at rest, prefixed, scoped, optionally IP-restricted and
  expiring; revocable instantly.
- **mTLS** — mutual certificate auth for east-west traffic in zero-trust networks.

External IdP integration (Okta, Entra ID, Google, etc.) via OIDC lets enterprises
bring their own identity provider and SSO.

---

# 4. Token Security

| Control | Policy |
|---------|--------|
| Access token TTL | Minutes (short-lived) |
| Refresh token | Longer-lived, rotated on use, revocable |
| Storage | Server-side (BFF) / OS keychain (CLI); never browser localStorage |
| Signing | Asymmetric keys, rotated on schedule, JWKS published |
| Audience binding | Tokens scoped to `apex-api` |

Compromised tokens are contained by short TTLs, rotation, and a revocation
blocklist.

---

# 5. Service-to-Service

Internal calls use **mTLS** plus short-lived service-identity JWTs minted per
workload (e.g. SPIFFE-style). No service holds long-lived shared secrets; identity
is cryptographically attested.

---

# 6. Multi-Factor & Step-Up

- MFA is delegated to the IdP (TOTP, WebAuthn, push).
- Sensitive operations (e.g. key creation, destructive admin) can require
  **step-up** re-authentication enforced via Policy Engine conditions.

---

# 7. Session Management

- Browser sessions are http-only, secure, same-site cookies issued by the
  [Dashboard BFF](../10-dashboard/overview.md#5-authentication-flow).
- Sessions are revocable; deactivating a user invalidates active sessions and keys.

---

# 8. Failure Handling

Authentication is **fail-closed**: invalid, expired, or unverifiable credentials
are rejected with `unauthenticated` and audited. Repeated failures trigger
rate-limiting and alerting (possible credential-stuffing).

---

# 9. Audit

Every authentication event (success, failure, token refresh, key use) is recorded
per [audit.md](audit.md) with principal, method, and source.

---

# 10. Dependencies

- [`09-api/authentication.md`](../09-api/authentication.md)
- [`13-security/secret-management.md`](secret-management.md)
- [`13-security/audit.md`](audit.md)

---

# 11. Related Documents

- [`13-security/authorization.md`](authorization.md)
- [`13-security/rbac.md`](rbac.md)

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.2.0 | 2026-07-15 | Status note refreshed for RM-AIM-P1 SRV-104 (API-key TTL/revoke/rotate lifecycle shipped); remaining gap narrowed to SSO/mTLS/MFA/sessions/JWT revocation. No design content changed |
| 1.1.0 | 2026-07-07 | Added a top note distinguishing this doc's target-state design from the real, fail-closed-by-default implementation (RM-GA-P1 SEC-101/SEC-102). Found during a project-wide status review; no design content changed |
| 1.0.0 | 2026-06-27 | Initial Security Authentication specification |
