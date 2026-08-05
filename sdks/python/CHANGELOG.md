# Changelog — `wovyr-sdk`

The SDK's version tracks the platform release it targets: same major.minor as
`wovyr-server` means same API surface (DX-303). Patch versions are SDK-only
fixes. `health()` warns (once per client) when it detects a server whose
major.minor differs.

## 0.4.1 — 2026-08-05

- Version-only bump to stay in lockstep with the platform's 0.4.1 release. No
  SDK code change — that release fixes the release pipeline itself, including
  the PyPI publish step, which had never once run: it gated on a secret from
  within `if:`, which cannot work, so every `wovyr-sdk` version on PyPI to date
  was published by hand. 0.4.1 is the first version this pipeline publishes.

## 0.4.0 — 2026-08-01

- Version-only bump to stay in lockstep with the platform's 0.4.0 release. No
  SDK code change: the API surface is identical to 0.3.2, and the platform's
  minor bump is a `wovyr-tools` `wasi`-feature break that no client sees. The
  major.minor tracking rule (DX-303) is what forces the bump — a 0.3.x SDK
  talking to a 0.4.x server would otherwise warn about skew that isn't real.

## Unreleased

Ordering caveat, pre-dating this release: the entries below are still filed
under "Unreleased" but shipped in an earlier `0.3.x` release — `0.3.1` and
`0.3.2` were published to PyPI without being recorded here. Left as-is rather
than re-filed under a guessed version.

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
