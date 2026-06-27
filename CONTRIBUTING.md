# Contributing to Apex AI Platform

Thanks for your interest in contributing to the **Apex AI Platform**.

The full, authoritative contributor guide lives in the documentation:

- **[Contributing](docs/19-implementation-guide/contributing.md)** — workflow, PRs, review, security disclosure
- **[Development Environment](docs/19-implementation-guide/development-environment.md)** — toolchain & local run
- **[Build System & SDK](docs/19-implementation-guide/build-system.md)** — workspace, builds, SDK
- **[Coding Standards](docs/19-implementation-guide/coding-standards.md)** — style, errors, logging, tests
- **[Release Process](docs/19-implementation-guide/release-process.md)** — versioning & releases

New to the project? Start with the [documentation index](docs/SUMMARY.md) and the
[Vision](docs/00-executive/vision.md).

---

## Quick start

```bash
git clone <repo> && cd apex
make setup        # toolchain + hooks
make dev          # run the all-in-one platform locally
make test         # unit + integration
```

See [Development Environment](docs/19-implementation-guide/development-environment.md)
for details.

---

## Ground rules (summary)

- **Match the surrounding code.** Consistency over preference.
- **Ship tests with code.** Every change includes tests at the right level
  ([Testing](docs/15-testing/index.md)).
- **Architectural changes need an [ADR](docs/17-adr/index.md)** before implementation.
- **Update docs/specs** affected by your change; update
  [`docs/SUMMARY.md`](docs/SUMMARY.md) when adding or removing documents.
- **Never commit secrets**; keep least-privilege in mind
  ([Security](docs/13-security/index.md)).

---

## Reporting security issues

**Do not** open public issues for vulnerabilities. Follow the responsible-disclosure
process described in
[Security Testing §9](docs/15-testing/security-testing.md#9-penetration-testing--reviews).

---

## License

By contributing, you agree that your contributions are licensed under the
[Apache License 2.0](LICENSE).
