<!--
File: docs/17-adr/ADR-0007-plugin-system.md
Document ID: ADR-0007
-->

# ADR-0007: Plugin-First Extensibility

**Status:** Accepted  
**Date:** 2026-06-27  
**Deciders:** Architecture Team  
**Supersedes:** —

---

# Context

The platform must be extensible by third parties and internal teams — new tools,
providers, memory backends, policies, and workflow activities — without forking or
redeploying the core. This is the "Plugin First" principle from the
[Vision](../00-executive/vision.md).

---

# Decision

Adopt a **plugin-first** model: capabilities are delivered as versioned, signed
**plugins** loaded at runtime by the **Plugin Engine**, with **WASM as the default**
isolation/loading model. Specified in [Plugin SDK](../08-plugin-sdk/index.md).

- Declared, least-privilege [permissions](../08-plugin-sdk/permissions.md), granted
  explicitly per tenant.
- Untrusted plugins run [sandboxed](../08-plugin-sdk/sandbox.md); tool plugins reuse
  the [Tool Runtime](../07-tool-runtime/index.md) isolation.
- Signed packages with provenance/SBOM and revocation
  ([distribution](../08-plugin-sdk/distribution.md)).
- Hot lifecycle (install/enable/upgrade) without core restarts
  ([versioning](../08-plugin-sdk/versioning.md)).

---

# Consequences

**Positive**
- Ecosystem and marketplace become possible
  ([marketplace](../08-plugin-sdk/marketplace.md)); core stays small.
- Capabilities ship and version independently of platform releases.
- Strong security posture via sandboxing + least-privilege grants.

**Negative**
- Significant infrastructure: signing, verification, dependency resolution,
  sandboxing, host interfaces.
- WASM constrains plugin languages/APIs vs. native (accepted for safety/portability;
  native loading reserved for first-party).
- Versioning/compatibility management is ongoing operational work.

---

# Alternatives Considered

- **Compile-time extensions only** — simplest, safest, but defeats third-party
  extensibility and the marketplace goal. Rejected.
- **Native dynamic libraries (dlopen)** — high performance but unsafe for untrusted
  code and not portable. Reserved only for trusted first-party plugins.
- **Out-of-process only (gRPC sidecars)** — strong isolation but heavier per-plugin
  cost; used for heavy/untrusted cases, with WASM as the lightweight default.

---

# Related

- [`08-plugin-sdk/index.md`](../08-plugin-sdk/index.md)
- [`08-plugin-sdk/sandbox.md`](../08-plugin-sdk/sandbox.md)
- [`07-tool-runtime/index.md`](../07-tool-runtime/index.md)
