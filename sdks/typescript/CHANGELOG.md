# Changelog — `@wovyr/sdk`

The SDK's version tracks the platform release it targets: same major.minor as
`wovyr-server` means same API surface (DX-303). Patch versions are SDK-only
fixes. `health()` warns (once per client) when it detects a server whose
major.minor differs.

## 0.3.1 — 2026-07-27

- Docs: removed the stale "Known gaps" note claiming the package was not yet
  published to npm (it is — `@wovyr/sdk` is live on the registry).

## Unreleased

- **Opt-in mutation retry** (DX-301): mutating requests that carry an
  `Idempotency-Key` (pass `idempotencyKey` in the call's `opts`) now retry
  transient failures (429/502/503/504, network errors) exactly like `GET`s —
  the server's replay middleware makes the retry safe. Keyless mutations still
  never retry.
- **`workflows.waitForCompletion(id, {intervalMs, timeoutMs})`** (DX-301):
  polls to a terminal status (`completed`/`failed`/`cancelled`) and returns
  the final snapshot; throws the new `WovyrTimeoutError` on deadline.
- **Version handshake** (DX-303): `health()` compares the server's version
  against `SDK_VERSION` and `console.warn`s on a major.minor skew — once per
  client, never thrown. `SDK_VERSION` and `versionSkew()` are exported.
- Type fixes: `AuditEntry` now matches the real wire envelope
  (`{id, seq, event, prev_hash, hash}` — the event nests under `event`);
  `Attestation.sbom`/`provenance` are precisely typed; added `Health`,
  `WorkflowSummary`, `MarketplaceListing`, tenancy shapes, and `Webhook`.

## 0.3.0 — 2026-07-15

- Version aligned to the platform's 0.3.0 (previously `0.1.0`).
- Everything shipped through v1.0–v1.3 platform work: full resource coverage
  (agents, workflows, memory, plugins, marketplace, secrets, organizations,
  projects, webhooks, audit, tools, ui, mcp), SSE streaming for
  `agents.stream`, GET retry with exponential backoff, `paginateAll`,
  `Idempotency-Key` support on every mutating route, ETag/`If-Match` project
  concurrency, and the generative-UI frame/decision endpoints.
