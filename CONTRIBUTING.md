# Contributing to Wovyr AI Platform

Thanks for your interest in contributing to the **Wovyr AI Platform**.

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
git clone <repo> && cd wovyr
make setup        # toolchain + hooks
make dev          # run the all-in-one platform locally
make test         # unit + integration
```

See [Development Environment](docs/19-implementation-guide/development-environment.md)
for details.

---

## Developer Certificate of Origin (DCO)

Wovyr is licensed under [Apache 2.0](LICENSE). To keep the provenance of every
contribution clear, we require the [Developer Certificate of Origin](https://developercertificate.org/)
instead of a CLA: by signing off a commit you certify you have the right to
submit it under the project's license.

Sign off every commit:

```bash
git commit -s
```

which appends a line like:

```
Signed-off-by: Your Name <your.email@example.com>
```

Use your real name and a reachable email (GitHub's `users.noreply.github.com`
addresses are fine). Pull requests with unsigned commits can't be merged;
`git rebase --signoff HEAD~N` retrofits sign-offs onto existing commits.

## Security issues

Never report vulnerabilities in public issues or PRs — see
[SECURITY.md](SECURITY.md) for the private reporting channel.

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
