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

You need **Rust 1.85+** (edition 2024). Nothing else is required to build, test, or
run the platform — everything below works offline against a deterministic mock
provider, with no API key.

```bash
git clone https://github.com/punarduttrajput/wovyr && cd wovyr
make setup        # rustfmt + clippy + wasm32-wasip1 target, then build the workspace
make test         # cargo test --workspace
make lint         # clippy -D warnings + fmt --check (what CI gates on)
make dev          # run the all-in-one local server on 127.0.0.1:8080
make run-hello    # or: run a single agent end to end
```

`make` is a thin wrapper — every target is one or two `cargo` commands, listed in the
[`Makefile`](Makefile) if you'd rather run them directly.

Working on the dashboard instead? `make dashboard-dev` (Angular; needs Node 20+). It
builds its `@wovyr/ui-react` dependency automatically.

**A note on the offline cargo config.** If `cargo build` fails with
`attempting to make an HTTP request, but --offline was specified`, your
`~/.cargo/config.toml` has `[net] offline = true`. Override it for the first build,
which needs to populate the dependency cache:

```bash
cargo build --workspace --config net.offline=false
```

See [Development Environment](docs/19-implementation-guide/development-environment.md)
for details, and [Build System](docs/19-implementation-guide/build-system.md) for the
workspace layout.

---

## Finding something to work on

The workspace is large (22 crates), so start narrow rather than reading it all:

- **Fixing a bug you hit?** That's the best first contribution. Reproduce it in a test
  first — the house convention is that a fix ships with a test that fails against the
  pre-fix code.
- **Want orientation?** [README's "Where to start"](README.md) names the four crates
  that make up the flagship surface. Each crate's `lib.rs` doc comment links the
  `docs/` section it implements.
- **Looking for scoped work?** [`docs/18-roadmap/`](docs/18-roadmap/index.md) is the
  ticket backlog: each entry states the problem with file:line evidence, the change,
  acceptance criteria, and a size estimate. Anything marked `S` is a reasonable first
  pass. `docs/18-roadmap/future/` holds larger, less-specified ideas.
- **Docs count.** `docs/` is the source of truth and parts of it still describe
  milestones that aren't built. Corrections that bring a doc in line with the code are
  genuinely valuable — see the "honest docs" rule below.

If you're unsure whether a change is wanted, open an issue describing it before
writing the code.

---

## Honest docs

This repo tries hard not to claim more than it does. Concretely, when you change
behavior:

- If a feature is partially built, say which part. Prefer "X works for Y; Z is not
  implemented" over an unqualified claim.
- If something is proven by a test, cite the test. If it was checked by hand, say
  "manually spot-checked, not CI-gated" rather than implying coverage.
- If you find a doc that overstates reality, fixing it is a welcome contribution on
  its own.

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
