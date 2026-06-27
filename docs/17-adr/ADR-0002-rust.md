<!--
File: docs/17-adr/ADR-0002-rust.md
Document ID: ADR-0002
-->

# ADR-0002: Rust as the Implementation Language

**Status:** Accepted  
**Date:** 2026-06-27  
**Deciders:** Architecture Team  
**Supersedes:** —

---

# Context

The platform runs long-lived, concurrent, security-sensitive services (runtime,
workflow engine, sandboxed tool execution) where performance, memory safety, and
predictable resource use matter. We must pick a primary implementation language.

---

# Decision

Use **Rust** for all backend services and SDKs ("Rust First", per the
[Vision](../00-executive/vision.md)). The dashboard frontend uses Angular/NestSJS
(a deliberate exception for the web tier).

Rationale:
- **Memory safety without GC** → no GC pauses, predictable latency for hot paths
  (gateway overhead, retrieval, scheduling).
- **Fearless concurrency** → safe high-concurrency services.
- **Performance** → meets aggressive [NFR targets](../07-tool-runtime/overview.md#9-non-functional-requirements).
- **Strong type system** → encodes invariants (DSL, state machine) at compile time.
- **Single static binaries** → simple, small [container images](../12-deployment/docker.md).

---

# Consequences

**Positive**
- High performance and safety; small, dependency-free deployables.
- Compile-time guarantees reduce whole classes of runtime bugs.
- Excellent for sandboxing/WASM host integration
  ([Tool Runtime](../07-tool-runtime/sandbox-runtime.md)).

**Negative**
- Steeper learning curve; smaller hiring pool than mainstream GC languages.
- Longer compile times (mitigated by workspace caching, `nextest`).
- Some AI/ML ecosystem libraries are younger than Python equivalents — mitigated by
  the [Provider SDK](../04-agent-framework/provider-sdk.md) abstraction over HTTP
  provider APIs rather than in-process ML.

---

# Alternatives Considered

- **Go** — simpler, good concurrency, but GC pauses and weaker type guarantees for
  the DSL/state invariants. Rejected for latency-critical core.
- **Python** — rich AI ecosystem but unsuitable for high-throughput, low-latency,
  safe concurrent services. Used (if at all) only at the edges, not the core.
- **JVM (Kotlin/Java)** — capable but heavier runtime/footprint and GC. Rejected.

---

# Related

- [`00-executive/vision.md`](../00-executive/vision.md) · [ADR-0001](ADR-0001-project-structure.md)
