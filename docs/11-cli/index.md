<!--
File: docs/11-cli/index.md
Document ID: CLI-INDEX-001
-->

# CLI Index

**Document ID:** CLI-INDEX-001  
**File Path:** `docs/11-cli/index.md`  
**Version:** 1.1.0  
**Status:** Active  
**Owner:** AI Platform Team  
**Last Updated:** 2026-08-01

---

# 1. Purpose

This document is the **central navigation and architecture index** for the `wovyr` command-line interface — the primary terminal client for the Wovyr AI Platform.

The CLI serves developers, operators, and CI/CD. It is both a **client of the
[Platform API](../09-api/index.md)** (managing remote resources) and a **local
toolchain** (scaffolding projects, building plugins, running workflows locally,
diagnostics) — see the CLI Service in
[C4 Container §4.10](../02-architecture/c4-container.md).

---

# 2. What the CLI Does

| Mode | Examples |
|------|----------|
| Remote management | `wovyr agents run`, `wovyr auth create-key` |
| Local development | `wovyr dev`, `wovyr plugin new`, `wovyr workflows run --local` |
| Authoring/build | `wovyr plugin build`, `wovyr plugin sign`, `wovyr plugin publish` |
| Operations | `wovyr admin backup`, `wovyr admin migrate`, `wovyr kms rotate` |

The [command reference](commands.md) is the authority here — it is generated
from the real command tree and diffed in CI, so it can never list a command that
doesn't exist.

**The CLI is not at parity with the Platform API**, and isn't trying to be: it
covers local development, plugin authoring, and node operations. Managing remote
resources (registering agents, submitting workflows, projects, quotas, webhooks,
audit) is done through the [API](../09-api/index.md) or an
[SDK](../../sdks), not the terminal.

---

# 3. Composition

```text
wovyr (Rust binary)
   │
   ├── command parser + help (clap)
   ├── credential store (~/.wovyr/credentials.json)
   ├── auth (bearer token; `auth` mints the server's API keys)
   ├── HTTP client (REST → the server)
   ├── local engine (embedded agent/workflow runtime for --local)
   └── local stores (~/.wovyr: kms, secrets, memory, workflows, plugins, ...)
```

The CLI is a single self-contained Rust binary
([tech mapping](../02-architecture/c4-container.md): CLI = Rust).

---

# 4. Document Map

| Document | Responsibility |
|----------|----------------|
| [installation.md](installation.md) | Install methods, platforms, updates |
| [configuration.md](configuration.md) | State directory, auth, environment variables |
| [commands.md](commands.md) | Full command reference (generated, CI-diffed) |
| [examples.md](examples.md) | Task-oriented recipes and CI usage |

---

# 5. Design Principles

1. **Local-first dev** — scaffold, build, and run with no remote at all; with no
   provider key set, runs use a deterministic mock so they work offline.
2. **Secure by default** — same authn/authz as every client, no privileged
   backdoor, and the privileged builtins (`shell`, `fs_write`, `code_execute`)
   fail closed without an explicit per-run or per-session opt-in (SBX-305).
3. **Generated reference** — `commands.md` comes from the clap tree, so the docs
   cannot drift from the binary.
4. **Helpful** — rich `--help` on every subcommand.

Deliberately *not* principles today: full API parity (§2), a machine-readable
`--output json` mode, graded exit codes, and shell completion. See
[configuration §6](configuration.md#6-output--exit-codes).

---

# 6. Dependencies

- [`09-api/index.md`](../09-api/index.md) — the API the CLI consumes
- [`09-api/authentication.md`](../09-api/authentication.md) — login + tokens
- [`08-plugin-sdk/plugin-api.md`](../08-plugin-sdk/plugin-api.md) — `wovyr plugin` build/publish

---

# 7. Related Documents

- [`10-dashboard/index.md`](../10-dashboard/index.md) — the other primary client
- [`12-deployment/index.md`](../12-deployment/index.md) — deploying the server the CLI talks to
- [`19-implementation-guide/development-environment.md`](../19-implementation-guide/development-environment.md)

---

# 8. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial CLI Index |
