<!--
File: docs/17-adr/ADR-0009-keyless-signing.md
Document ID: ADR-0009
-->

# ADR-0009: Apex-Native Keyless Signing (Sigstore-Shaped, Offline-Verifiable)

**Status:** Accepted
**Date:** 2026-07-03
**Deciders:** Platform Security Team
**Supersedes:** — (complements the ed25519 `TrustStore` mode, which remains)

---

# Context

Plugin packages are signed with **long-lived publisher ed25519 keys** verified
against a `TrustStore` ([`apex-plugin/src/verify.rs`](../../crates/apex-plugin/src/verify.rs)).
Long-lived keys are the supply chain's weakest link: they leak, they outlive the
people who held them, and revocation is manual. The v0.3 security track calls for
**keyless (identity-based) signing** in the Sigstore style: a certificate authority
(Fulcio's role) binds an OIDC identity to a *short-lived* certificate over an
*ephemeral* key; a transparency log (Rekor's role) publicly witnesses each signing
event; verifiers trust a pinned root, not per-publisher keys.

Adopting the real Sigstore stack wholesale conflicts with this codebase's
constraints:

- **Determinism** ([coding-standards §7]): core logic takes no ambient clock or
  network. Sigstore verification libraries fetch TUF roots and check X.509 validity
  against wall clock.
- **Offline-first development**: the public `fulcio/rekor.sigstore.dev` are not
  reachable from CI or the dev environment reliably; X.509/Fulcio parsing pulls a
  heavy dependency tree.
- The platform already has the primitives the design needs: ed25519
  sign/verify (`ring`), a hash-chained tamper-evident audit log (`apex-audit`),
  and the capability-gated live-test pattern for real infrastructure.

# Decision

Implement **Apex-native keyless signing** in
[`apex-plugin/src/keyless.rs`](../../crates/apex-plugin/src/keyless.rs): the
Sigstore *architecture* (ephemeral key → short-lived identity certificate →
transparency-log witness → pinned-root verification) with Apex-native encodings
(canonical delimiter-separated byte strings signed by ed25519, JSON on the wire)
instead of X.509/DER.

1. **Ports, not services.** `CertificateAuthority` and `TransparencyLog` are
   traits. `InMemoryCa`/`InMemoryTransparencyLog` are the deterministic in-process
   implementations (tests, single-operator dev). A Rekor-backed log
   ([`rekor.rs`](../../crates/apex-plugin/src/rekor.rs), `rekor` cargo feature)
   appends `rekord` entries to a real Rekor server; `deployment/rekor/` runs one
   locally (pinned release images, no source builds), live-tested behind
   `APEX_REKOR_URL` (`tests/rekor_live.rs`).
2. **Verification is fully offline and clock-free.** A `KeylessBundle`
   (certificate + manifest signature + log entry) is self-contained; verifiers hold
   a pinned `KeylessRoot` (CA + optional log public keys). The certificate-validity
   check anchors on the **log's `integrated_time`**, never a local clock, so
   `verify_keyless` is deterministic over its inputs. Certificates are backdated by
   a 60 s skew allowance (Rekor timestamps are whole seconds).
3. **Identity → namespace binding, fail-closed.** An `IdentityPolicy` maps
   identities (`issuer` exact, `subject` prefix-wildcard) to the **publisher
   namespaces** they may sign for, checked against the manifest's declared
   publisher — an allowed identity cannot sign for someone else's namespace. Empty
   policy admits nobody. A transparency-log entry is required unless the operator
   opts out (`require_transparency: false`, the registry-witnessed mode).
4. **Bundles ride in the `.apexpkg` envelope** (optional `keyless` field), and
   `Package::verify_keyless(root, policy)` is the counterpart to the existing
   `Package::verify(trust)`. Both trust modes coexist; consumers choose per policy.

# Consequences

- **(+)** Verification needs zero network and no new dependencies; the full
  matrix (tamper, unpinned CA, policy denial, expired window, forged SET, missing
  entry) is unit-tested offline; the Rekor path is proven live against real
  infrastructure.
- **(+)** Publishers need no long-lived signing secret; compromise windows shrink
  to the certificate TTL (default 10 min).
- **(−)** Not wire-compatible with Sigstore tooling (`cosign` cannot verify an
  Apex bundle). Acceptable: the trust root is operator-pinned either way, and the
  architecture leaves room to swap encodings later.
- **(+)** Rekor's **signed entry timestamp is verified offline**: the bundle
  carries Rekor's canonicalized entry `body`, the verifier reproduces the RFC 8785
  SET payload (`{body, integratedTime, logID, logIndex}`), and pinned log keys are
  accepted as raw/SPKI ed25519 or ECDSA P-256 (Rekor's memory signer) — proven
  live against a real Rekor (forged coordinates are rejected).
- **(−)** No real OIDC yet: the CA attests whatever identity the operator of the
  signing environment asserts. Fine for the dev CA; a Fulcio-compatible or
  OIDC-validating CA is additive behind the same trait.

# Consumers (landed after the core)

Keyless is a policy-selectable trust mode at both supply-chain choke points, with
identical no-downgrade semantics (a present bundle is verified keylessly or
rejected — never falls back to the publisher-key path):

- **Registry publish** — `Registry::with_keyless(root, policy)`
  (`apex-marketplace`); a keyless-only package flows publish → discover →
  download → install → enable with no publisher key anywhere
  (`tests/keyless_supply_chain.rs`).
- **Engine install** — `PluginEngine::with_keyless(root, policy)` (`apex-plugin`).
- **Server** — both are configured from one operator file,
  `~/.apex/plugins/keyless.json` (`{"root": …, "policy": …}`); absent ⇒ keyless
  disabled.

- **CLI tooling** — `apex plugin keyless-init` (dev CA + pinned trust config,
  shared with the server) and `apex plugin keyless-sign` (ephemeral key never
  touches disk; `--rekor <url>` witnesses the signing, behind the CLI's
  `keyless-rekor` feature). `plugin.keyless.json` beside a manifest rides into
  packages via `pack`/`install`/`publish`, and `plugin.sig` becomes optional for
  keyless-only packages.

# Deferred

- Merkle inclusion-proof checks against the log's signed tree head.
- X.509/Fulcio certificate compatibility; OIDC token validation in the CA
  (a non-goal while the trust root is operator-pinned — revisit if `cosign`
  interop is ever required).
- Audit-log recording of keyless publish events.
