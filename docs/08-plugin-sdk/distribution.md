<!--
File: docs/08-plugin-sdk/distribution.md
Document ID: PLG-006
-->

# Plugin Distribution

**Document ID:** PLG-006  
**File Path:** `docs/08-plugin-sdk/distribution.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines how plugins are **packaged, signed, published, and pulled** — the supply chain from a developer's build to a verified install on the platform.

Distribution is where trust is established: every package must be verifiable, tamper-evident, and traceable to its publisher before any code runs.

---

# 2. Package Format (`.wovyrpkg`)

A plugin is distributed as a single content-addressed archive:

```text
github-1.4.0.wovyrpkg
├── plugin.yaml          # manifest
├── artifacts/           # wasm / binaries / images (each digest-pinned)
├── schemas/             # input/output/config JSON schemas
├── LICENSE
├── README.md
├── SBOM.json            # software bill of materials
└── MANIFEST.sig         # detached signature over a digest manifest
```

The archive is identified by the digest of its **digest manifest** (a list of every
file and its hash), so any byte change yields a new identity.

---

# 3. Signing

```text
Build → compute digest manifest → sign manifest with publisher key → attach MANIFEST.sig
```

- Publishers sign with a key bound to their marketplace identity.
- Keyless/transparency-log signing (Sigstore-style) is supported so signatures are
  publicly verifiable without distributing keys.
- The signature covers the **whole** package (manifest + all artifacts), so partial
  tampering is detectable.

---

# 4. Provenance & SBOM

- Each package carries an **SBOM** listing dependencies and their versions.
- Build **provenance** (who/what/when built it, from which source) is recorded and
  attestable, enabling supply-chain policies like "only allow plugins built by
  trusted CI."
- Provenance and SBOM are checked at install per tenant policy.

---

# 5. Registries

| Registry | Use |
|----------|-----|
| Public Marketplace | Community + verified publishers (see [Marketplace](marketplace.md)) |
| Private registry | Org-internal plugins, not publicly listed |
| Mirror | Cached copy for air-gapped/enterprise deployments |
| Local file | Direct install from a `.wovyrpkg` (dev/testing) |

A deployment can configure multiple sources with precedence (e.g. private over
public) and an allowlist of trusted publishers.

---

# 6. Publish Flow

```text
wovyr plugin publish
   │
   ▼
Validate manifest + schemas + SBOM
   │
   ▼
Verify signature + provenance
   │
   ▼
Run automated checks (compat, lint, optional security scan)
   │
   ▼
Store artifacts (content-addressed) + index version
   │
   ▼
Emit plugin.published
```

Publishing to `stable` may require passing automated checks and (for the public
marketplace) review — see [Marketplace §6](marketplace.md#6-review--quality).

---

# 7. Install / Pull Flow

```text
Plugin Engine pull
   │
   ▼
Fetch package by name@version from configured registry
   │
   ▼
Verify digest manifest + signature + provenance + SBOM policy
   │
   ▼
Check publisher allowlist + platform compatibility
   │
   ▼
Stage artifacts (verified by digest) → ready to register
```

Verification is **mandatory and fail-closed**: an unverifiable or
policy-violating package is never staged. This mirrors the
[Tool Runtime supply-chain checks](../07-tool-runtime/security-isolation.md#9-supply-chain--provenance).

---

# 8. Revocation

- A publisher or the platform can **revoke** a version (compromised key, critical
  CVE). Revocation is distributed via a signed revocation list.
- The Plugin Engine checks the revocation list on install and periodically for
  installed plugins; a revoked version is **force-disabled** and operators alerted.
- Revocation composes with [Versioning §9](versioning.md#9-deprecation--retirement).

---

# 9. Air-Gapped Distribution

- An enterprise mirror is populated from the public marketplace (or curated) and
  serves installs without internet access.
- Signatures and provenance are verified against mirrored trust roots.
- Revocation lists are imported on a controlled cadence.

---

# 10. Integrity Guarantees

| Guarantee | Mechanism |
|-----------|-----------|
| Authenticity | Publisher signature |
| Integrity | Content-addressed digest manifest |
| Traceability | Build provenance attestation |
| Transparency | Public signing/transparency log |
| Composition visibility | SBOM |
| Recall | Signed revocation list |

---

# 11. Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Signature verification | < 100 ms |
| Provenance/SBOM policy check | < 200 ms |
| Pull (cached) | < 500 ms |
| Revocation propagation | minutes |

---

# 12. Dependencies

- [`08-plugin-sdk/plugin-api.md`](plugin-api.md)
- [`08-plugin-sdk/versioning.md`](versioning.md)
- [`07-tool-runtime/security-isolation.md`](../07-tool-runtime/security-isolation.md#9-supply-chain--provenance)
- [`08-plugin-sdk/marketplace.md`](marketplace.md)

---

# 13. Related Documents

- [`08-plugin-sdk/overview.md`](overview.md)
- [`08-plugin-sdk/permissions.md`](permissions.md)

---

# 14. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Plugin Distribution specification |
