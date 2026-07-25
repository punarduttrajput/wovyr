<!--
File: docs/19-implementation-guide/index.md
Document ID: IMPL-INDEX-001
-->

# Implementation Guide Index

**Document ID:** IMPL-INDEX-001  
**File Path:** `docs/19-implementation-guide/index.md`  
**Version:** 1.0.0  
**Status:** Active  
**Owner:** Engineering Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This section is the **contributor's handbook** — how to set up a dev environment,
build and test, follow coding standards, cut releases, and contribute. It turns the
architecture and specs into day-to-day engineering practice.

---

# 2. Audience

New and existing contributors to the Wovyr AI Platform monorepo
([ADR-0001](../17-adr/ADR-0001-project-structure.md)).

---

# 3. Document Map

| Document | Responsibility |
|----------|----------------|
| [development-environment.md](development-environment.md) | Toolchain, prerequisites, local run |
| [build-system.md](build-system.md) | Workspace, builds, the Rust SDK |
| [coding-standards.md](coding-standards.md) | Style, error handling, logging, tests |
| [release-process.md](release-process.md) | Versioning, releases, signing |
| [contributing.md](contributing.md) | Workflow, reviews, security disclosure |

---

# 4. Quick Start

```bash
git clone <repo> && cd wovyr
make setup          # toolchain + hooks
make dev            # run the all-in-one platform locally
make test           # unit + integration
```

Details in [Development Environment](development-environment.md) and
[Build System](build-system.md).

---

# 5. Principles

1. **Match the surrounding code** — consistency over personal preference.
2. **Tests with code** — every change ships tests ([section 15](../15-testing/index.md)).
3. **Determinism** — no hidden clocks/randomness in core logic.
4. **Security-aware** — least privilege, no secrets in code/logs.
5. **Docs follow decisions** — update specs/ADRs when design changes.

---

# 6. Related

- [`17-adr/index.md`](../17-adr/index.md) · [`15-testing/index.md`](../15-testing/index.md)
- [`12-deployment/index.md`](../12-deployment/index.md)

---

# 7. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Implementation Guide index |
