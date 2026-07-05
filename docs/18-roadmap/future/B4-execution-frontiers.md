<!--
File: docs/18-roadmap/future/B4-execution-frontiers.md
Document ID: FUT-004
-->

# Future Exploration: Execution Frontiers

**Document ID:** FUT-004
**File Path:** `docs/18-roadmap/future/B4-execution-frontiers.md`
**Version:** 1.0.0
**Status:** Exploratory — research bet, not committed
**Owner:** Tool Runtime Team
**Last Updated:** 2026-07-05

---

# 1. Purpose

Flesh out the "Execution Frontiers" research bet
([future.md §2.4](../future.md#24-execution-frontiers),
[PRD-002 §6.4](../../01-product/prd-future.md#64-execution-frontiers)):
faster cold starts, GPU-aware scheduling, edge/regional inference, and a WASM
component model — all riding the existing sandbox isolation contract, never
around it.

Exploratory — graduates only via an [ADR](../../17-adr/index.md).

---

# 2. Problem & Opportunity

The tool runtime has a mature isolation spectrum
(native → wasm → container → gVisor → microVM), warm pooling, and egress
lockdown ([`apex-tools`](../../../crates/apex-tools/src/lib.rs)). The frontiers
are about **speed, hardware, locality, and portability** without giving up
isolation:

- **Snapshot/restore sandboxes** for near-instant cold starts (extending warm
  pooling — [tool-runtime futures](../../07-tool-runtime/overview.md#15-future-enhancements)).
- **GPU-aware scheduling** for model/inference workloads.
- **Edge/regional inference pools** for locality and residency.
- **A WASM component model** for portable, polyglot plugins.

---

# 3. Current Baseline (what this would build on)

- **Sandbox spectrum + trust floors** — `SandboxBackend`, `TrustClass`, and
  `SandboxManager::detect`/`select_backend` already pick the strongest of
  preference/floor/trust and check node capability. Any new backend slots in here.
- **Warm pooling + autoscaling** — `SandboxPool` (semaphore-bounded `acquire`,
  `PooledSandbox` return-on-drop, `AutoscalePolicy`) is the base that
  snapshot/restore accelerates.
- **microVM + WASI backends** — `FirecrackerSandbox` (one-shot block-device
  protocol) and the `wasi`-gated `WasiSandbox` (Wasmtime, fuel/epoch/memory
  limits) already exist; the WASM component model extends the latter and the
  `WasiCapabilityRuntime` in `apex-plugin`.
- **Fair scheduling** — the `FairScheduler` (smooth weighted round-robin) is
  where GPU-aware scheduling must integrate.

---

# 4. Direction (design sketch, non-committal)

- **Snapshot/restore:** capture a warmed sandbox's state and restore it on
  `acquire`, turning cold starts into restores. Extends `SandboxPool`, not a new
  isolation model.
- **GPU scheduling:** GPU as a schedulable resource *inside* the `FairScheduler`
  fairness model, not a side channel — so tenant fairness and admission still
  hold for GPU workloads.
- **Edge/regional pools:** pools tagged by region; placement honors residency
  (ties into ABAC/residency, [encryption §7](../../13-security/encryption.md)).
- **WASM component model:** move plugin capabilities from raw `wasm32-wasi`
  modules to the component model for portable, typed, polyglot interfaces.

---

# 5. Requirements

## 5.1 Functional
- A new backend is selectable through `SandboxManager` (preference/floor/trust +
  capability probe), never bypassing it.
- GPU workloads schedule through the `FairScheduler`'s admission/fairness.
- Snapshot/restore is transparent to callers (same `acquire` contract).

## 5.2 Invariants to preserve
- **Isolation floor is non-negotiable.** A faster or GPU-enabled backend may not
  lower the `TrustClass` floor an untrusted workload is held to.
- **Egress control holds** — a new backend still enforces `NetworkPolicy` /
  egress lockdown.
- **Determinism of scheduling** — the scheduler stays deterministic and
  caller-driven.

---

# 6. Key Risks & Open Questions

- **A faster backend weakening isolation** — the central risk; speed must not buy
  a weaker sandbox for untrusted code.
- **Snapshot state leakage** — a restored snapshot must not carry another
  tenant's residue.
- **GPU sharing isolation** — multi-tenant GPU use is a hard isolation problem.
- **Component-model maturity** — toolchain/runtime readiness across languages.

---

# 7. Graduation Gate

Per-backend; each becomes an ADR + roadmap slot only when:

> The new/faster backend **passes the adversarial escape and isolation test
> battery** (the v0.3 sandbox-escape precedent —
> [security-testing §5](../../15-testing/security-testing.md#5-sandbox--isolation-tests))
> *before* it is selectable, and (for snapshot/restore) proves no cross-tenant
> state carryover.

---

# 8. Dependencies

- The existing sandbox-escape test battery
  ([security-testing §5](../../15-testing/security-testing.md#5-sandbox--isolation-tests))
  — extended to each new backend as its gate.

---

# 9. Related Documents

- [`18-roadmap/future.md`](../future.md) §2.4 — origin
- [`01-product/prd-future.md`](../../01-product/prd-future.md) §6.4
- [`07-tool-runtime/overview.md`](../../07-tool-runtime/overview.md#15-future-enhancements)
- [`07-tool-runtime/security-isolation.md`](../../07-tool-runtime/security-isolation.md)

---

# 10. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-05 | Initial exploration doc for the execution-frontiers research bet |
