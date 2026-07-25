# Changelog — `wovyr-sdk`

The SDK's version tracks the platform release it targets: same major.minor as
`wovyr-server` means same API surface (DX-303). Patch versions are SDK-only
fixes. `health()` warns (once per client) when it detects a server whose
major.minor differs.

## Unreleased

- **Asyncio client** (DX-301): `wovyr_sdk.aio.AsyncWovyrClient` — the same
  resource surface with every method awaitable (sync transport delegated to a
  worker thread, so the event loop never blocks; `agents.stream` bridges SSE
  frames through an `asyncio.Queue` as they arrive), plus an async
  `paginate_all`.
- **Opt-in mutation retry** (DX-301): mutating requests that carry an
  `Idempotency-Key` (pass `idempotency_key=`) now retry transient failures
  (429/502/503/504, network errors) exactly like `GET`s — the server's replay
  middleware makes the retry safe. Keyless mutations still never retry.
- **`workflows.wait_for_completion(execution_id, interval_s=, timeout_s=)`**
  (DX-301): polls to a terminal status (`completed`/`failed`/`cancelled`) and
  returns the final snapshot; raises the new `WovyrTimeoutError` on deadline.
- **Version handshake** (DX-303): `health()` compares the server's version
  against the SDK's and emits an `WovyrVersionSkewWarning` on a major.minor
  skew — once per client, never raised. `sdk_version()` and `version_skew()`
  are exported.

## 0.3.0 — 2026-07-15

- Version aligned to the platform's 0.3.0 (previously `0.1.0`).
- Everything shipped through v1.0–v1.3 platform work: full resource coverage
  (agents, workflows, memory, plugins, marketplace, secrets, organizations,
  projects, webhooks, audit, tools), SSE streaming for `agents.stream`, GET
  retry with exponential backoff, `paginate_all`, `Idempotency-Key` support
  on every mutating route, and ETag/`If-Match` project concurrency — all on
  the standard library only (no runtime dependencies).
