<!--
File: docs/19-implementation-guide/contributing.md
Document ID: IMPL-005
-->

# Contributing

**Document ID:** IMPL-005  
**File Path:** `docs/19-implementation-guide/contributing.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Engineering Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document describes how to contribute to the Wovyr AI Platform — the workflow
from idea to merged change, review expectations, and community norms. It is the
canonical content behind the repository's `CONTRIBUTING.md`.

---

# 2. Before You Start

- Read the [Vision](../00-executive/vision.md), [Architecture](../02-architecture/c4-context.md),
  and relevant subsystem docs.
- Check the [roadmap](../18-roadmap/index.md) and existing issues to avoid duplication.
- Set up your [dev environment](development-environment.md).

---

# 3. Contribution Workflow

```text
Issue / proposal
   │
   ▼
Discuss (design + ADR if architectural)
   │
   ▼
Branch → implement + tests → self-review
   │
   ▼
PR → CI green → code review → merge
```

Architectural changes require an [ADR](../17-adr/index.md) before implementation.

---

# 4. Pull Requests

A good PR:
- Is focused and reasonably small; one logical change.
- Includes tests at the right level ([testing](../15-testing/index.md)).
- Passes CI (fmt, clippy, unit, integration).
- Updates docs/specs/ADRs affected by the change.
- Has a clear description: what, why, and how verified.

PRs must follow the [coding standards](coding-standards.md).

---

# 5. Code Review

- At least one maintainer approval; sensitive areas (auth, isolation, crypto)
  require a security reviewer.
- Reviews focus on correctness, security, clarity, and consistency with surrounding
  code.
- Be kind and specific; prefer suggestions over demands.

---

# 6. Commit Conventions

- Conventional commits (`feat:`, `fix:`, `docs:`, …) drive the
  [changelog](release-process.md#7-changelog--notes).
- Keep history clean; squash noise before merge.

---

# 7. Documentation Contributions

- Docs live in `docs/` and follow the existing format (metadata header, numbered
  sections, revision history) — see any current spec for the pattern.
- Update [SUMMARY.md](../SUMMARY.md) when adding/removing documents.
- The canonical product name is **Wovyr AI Platform** (short: **Wovyr**).

---

# 8. Reporting Issues

- Bugs: include repro, expected vs. actual, and the `request_id` if applicable.
- Features: describe the problem and use case before the solution.

---

# 9. Security Disclosure

**Do not** open public issues for vulnerabilities. Use the private security contact
/ responsible-disclosure process ([security testing](../15-testing/security-testing.md#9-penetration-testing--reviews)).
Coordinated disclosure and (where applicable) revocation follow.

---

# 10. Code of Conduct & License

- Contributors follow the project Code of Conduct.
- Contributions are licensed under the project license (Apache 2.0,
  per [README](../../README.md)).

---

# 11. Related

- [`19-implementation-guide/coding-standards.md`](coding-standards.md)
- [`19-implementation-guide/release-process.md`](release-process.md)
- [`17-adr/index.md`](../17-adr/index.md)

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Contributing guide |
