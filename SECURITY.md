# Security Policy

Wovyr is a trust and policy layer for AI-agent interfaces — we treat security
reports as first-class work, and we appreciate the time it takes to make one.

## Reporting a vulnerability

**Please do not open a public issue for security vulnerabilities.**

Report privately via **GitHub's private vulnerability reporting**:
[Security → Report a vulnerability](https://github.com/punarduttrajput/Wovyr/security/advisories/new)
on this repository. This is the fastest path to the maintainers and keeps the
report confidential while a fix is prepared.

Include what you can of:

- A description of the issue and its impact (which component: `wovyr-server`
  routes, sandbox escape, KMS/secrets, UI trust layer, plugin supply chain, …).
- Reproduction steps or a proof of concept.
- The commit/tag/version you tested against.

## What to expect

- **Acknowledgement** within 7 days.
- We'll work with you on validation and severity, and we'll credit you in the
  advisory unless you prefer otherwise.
- A fix and coordinated disclosure timeline agreed together — our default ask
  is 90 days or until a patched release ships, whichever is sooner.

## Scope notes

- The platform's own threat model and security architecture live in
  [`docs/13-security/`](docs/13-security/index.md). Divergences between a
  design document marked *target-state* and the shipped implementation are
  documented in each file's status header — a gap that's already documented
  there is a roadmap item, not a vulnerability, but **fail-closed claims the
  docs make about shipped code are in scope**: if something we say fails
  closed actually fails open, we want to know urgently.
- `disabled-loopback` auth mode, `WOVYR_ALLOW_ANONYMOUS=1`,
  `WOVYR_UNRESTRICTED_TOOLS=1`, and `WOVYR_UNRESTRICTED_UI=1` are documented
  trusted-local escape hatches; reports assuming them enabled on a public
  bind are out of scope (the server refuses that configuration — if it
  doesn't, *that* is in scope).

## Supported versions

Pre-1.0: security fixes land on `main` and the latest tagged release only.
