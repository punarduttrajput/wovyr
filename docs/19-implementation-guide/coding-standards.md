<!--
File: docs/19-implementation-guide/coding-standards.md
Document ID: IMPL-003
-->

# Coding Standards

**Document ID:** IMPL-003  
**File Path:** `docs/19-implementation-guide/coding-standards.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Engineering Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines the coding standards for the Wovyr AI Platform — consistent
style, error handling, logging, and testing practices so the codebase stays
readable and safe as it grows.

---

# 2. Guiding Principle

**Match the surrounding code.** Consistency with the existing module (naming,
structure, idioms) outweighs personal preference. New code should read like it was
always there.

---

# 3. Formatting & Linting

- `rustfmt` is canonical; CI fails on unformatted code.
- `clippy` runs with `-D warnings` — no warnings merged.
- Dashboard: ESLint + Prettier enforced.

These run in pre-commit hooks and CI ([build system](build-system.md#8-ci-build-pipeline)).

---

# 4. Naming & Structure

- Clear, descriptive names; no abbreviations that obscure intent.
- One responsibility per module; keep [Clean Architecture](../17-adr/ADR-0006-clean-architecture.md)
  boundaries — domain logic free of infrastructure types.
- Public APIs are documented with doc comments and examples.

---

# 5. Error Handling

- Use `Result` with typed, descriptive errors (`thiserror`-style); avoid `unwrap`/
  `panic` in service code (allowed in tests and truly-unreachable invariants).
- Map internal errors to the stable [API error envelope](../09-api/overview.md#8-error-model)
  at the boundary; preserve a correlation `request_id`.
- Authorization and verification paths are **fail-closed**
  ([authorization](../13-security/authorization.md#4-fail-closed)).

---

# 6. Logging

- Structured, leveled logs per [logging standards](../14-observability/logging.md):
  events not prose, variables in fields.
- **Never** log secrets or raw PII ([masking](../13-security/secret-management.md#9-masking)).
- Include `request_id`/`trace_id` for correlation.

---

# 7. Determinism

Core logic must be deterministic and testable: inject clocks, IDs, and randomness
rather than calling them ambiently ([unit testing](../15-testing/unit-tests.md#5-determinism-helpers)).
This is required for [workflow replay](../15-testing/workflow-tests.md#22-deterministic-replay).

---

# 8. Concurrency

- Prefer message passing and clear ownership; document shared-state invariants.
- No blocking calls in async contexts; bound concurrency explicitly.

---

# 9. Security Practices

- Least privilege everywhere; no ambient credentials.
- Validate all external input against schemas at boundaries.
- Treat tool/plugin code as untrusted ([isolation](../07-tool-runtime/security-isolation.md)).
- Changes to auth/isolation/crypto require a security review.

---

# 10. Tests with Code

Every change ships tests at the appropriate level
([testing](../15-testing/index.md)); bug fixes include a regression test. Coverage
gates apply to critical modules.

---

# 11. Documentation Discipline

- Update specs/ADRs when behavior or design changes
  ([ADRs](../17-adr/index.md)).
- Keep public API docs and examples current with the code.

---

# 12. Related

- [`19-implementation-guide/contributing.md`](contributing.md)
- [`15-testing/index.md`](../15-testing/index.md)
- [`14-observability/logging.md`](../14-observability/logging.md)

---

# 13. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Coding Standards |
