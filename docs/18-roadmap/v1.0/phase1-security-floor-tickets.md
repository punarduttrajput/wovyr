<!--
File: docs/18-roadmap/v1.0/phase1-security-floor-tickets.md
Document ID: RM-GA-P1
-->

# Phase 1 — Security Floor: Implementation Tickets

**Document ID:** RM-GA-P1
**File Path:** `docs/18-roadmap/v1.0/phase1-security-floor-tickets.md`
**Version:** 1.1.0
**Status:** Done — all 15 tickets shipped (SEC-101–105, SEC-201–205,
SEC-301–305). This doc was never updated when the work landed (commits
`8a9e4cb`, `64ab69d`, `8a48eb0`, `da5f0dd`, `8f3cb0e`, `9ca9228`) — a real
documentation-drift gap, caught and corrected 2026-07-07. See `CLAUDE.md`'s
`wovyr-server`/`wovyr-tools` bullets for the implemented behavior and
`crates/wovyr-server/src/{auth,rate_limit}.rs`/`crates/wovyr-tools/src/
{registry,builtin}.rs` for the code.
**Owner:** Engineering (Security)
**Last Updated:** 2026-07-07

---

# Purpose

Phase 1 of [PRD-003 §10](../../01-product/prd-ga-hardening.md) — the **P0 security
floor** — broken into implementation tickets. Nothing else ships to a network until
these close. Covers workstreams **WS-1** (auth), **WS-2** (transport/resource
hardening), **WS-3** (safe-by-default sandboxing).

Each ticket is copy-pasteable into an issue tracker: it states the problem with
file:line evidence, the change, acceptance criteria, the files it touches, its
dependencies, and a rough size (S ≈ ≤2 days, M ≈ 3–5 days, L ≈ 1–2 weeks).

**Legend:** `[P0]` GA blocker. `blocks:` / `depends on:` reference other ticket ids.

---

# Sequencing at a glance

```
SEC-101 (auth layer) ──┬─> SEC-102 (kill anon bypass) ──> SEC-105 (neg-auth CI gate)
                       ├─> SEC-103 (plugin routes authz) ─┘
                       └─> SEC-104 (marketplace authz) ────┘

SEC-201 (timeout/body/concurrency)  ─ independent, land first (cheap, high value)
SEC-202 (TLS)                        ─ independent
SEC-203 (rate limit) ── depends on SEC-101 (per-principal keying)
SEC-204 (CORS)                       ─ independent (coordinates with dashboard, WS-8)
SEC-205 (idempotency TTL)            ─ independent

SEC-301 (no default shell)           ─ independent
SEC-302 (fs_read confinement)        ─ independent
SEC-303 (deny-by-default grants)     ─ independent
SEC-304 (http_get SSRF guard)        ─ independent
SEC-305 (TrustClass on run path) ── depends on SEC-303 (shared run-context plumbing)
```

**Critical path:** SEC-101 → SEC-102/103/104 → SEC-105. Everything in WS-2 and WS-3
parallelizes against it. **Land SEC-101 first** — every other auth fix is inert
without a verified identity, and SEC-203 keys its rate limit on it.

---

# WS-1 — Authentication & Authorization

## SEC-101 `[P0]` — Introduce a credential verification layer

**Problem.** Identity is an unverified header. `context()`
(`crates/wovyr-server/src/tenancy.rs:80-86`) reads `X-Wovyr-Tenant` and
`X-Wovyr-Principal` verbatim, and treats an `Authorization: Bearer <t>` token as an
*opaque principal string* — it is never validated. `is_platform_admin`
(`tenancy.rs:66-71`) then matches that unverified string against
`WOVYR_PLATFORM_ADMINS`. Any caller can assert any principal, including platform
admin. (PRD-003 R-1.1; closes PP-01.)

**Change.**
- Add an auth middleware layer that verifies a credential *before* any handler runs
  and attaches a verified `Identity { principal, tenant, verified: true }` request
  extension.
- Support at least one real scheme for GA. Recommended: **signed JWT** (HS256/RS256,
  issuer + audience + expiry checked) validated against a configured key/JWKS, and/or
  **API keys** hashed (argon2/sha256) in a store, mapping key → principal + tenant.
  `ring` is already in the dependency graph (via `reqwest`'s `rustls-tls`).
- `context()` / `tenant_context()` derive principal + tenant from the verified
  `Identity`, never from raw headers. Raw `X-Wovyr-*` headers are honored only when
  they match the verified credential (or ignored entirely).
- Config via env: `WOVYR_AUTH_MODE` (`jwt` | `apikey` | `disabled-loopback`),
  `WOVYR_JWT_JWKS_URL`/`WOVYR_JWT_HS_SECRET`, `WOVYR_JWT_ISSUER`, `WOVYR_JWT_AUDIENCE`.

**Acceptance criteria.**
- A request with no/invalid credential to any non-public route → `401`.
- A request with a valid credential resolves the correct principal/tenant; forged
  `X-Wovyr-Principal` cannot override the verified identity.
- Public routes (`/healthz`, `/metrics`) remain unauthenticated (metrics gating is a
  separate WS-8 concern).
- Unit + integration tests for token accept/reject, expiry, wrong issuer/audience.

**Files.** `crates/wovyr-server/src/` — new `auth.rs`; `lib.rs` (router layer),
`tenancy.rs` (context derivation). New dep: a JWT crate (e.g. `jsonwebtoken`).
**Size.** L. **Depends on:** none (foundational). **Blocks:** SEC-102, SEC-103,
SEC-104, SEC-105, SEC-203.

**Status: Done.** `crates/wovyr-server/src/auth.rs`'s `authenticate` middleware
mounts over every route except `/healthz`/`/metrics` and verifies before any
handler runs: `AuthMode::Jwt` (HS256 via `WOVYR_JWT_HS_SECRET` or RS256 via
`WOVYR_JWT_RS_PUBLIC_KEY`, with optional issuer/audience checks) and
`AuthMode::ApiKey` (SHA-256-hashed lookup in a `FileApiKeyStore` at
`~/.wovyr/auth`) both overwrite `X-Wovyr-Principal` with the verified `sub`/
principal before the request reaches `tenancy::context`, so a forged header
can no longer win. `AuthMode::DisabledLoopback` (the default) is SEC-102's
concern. Proven by `jwt_hs256_accepts_a_valid_token_and_extracts_the_principal`,
`jwt_rejects_wrong_signature`, `jwt_rejects_expired_token`,
`jwt_rejects_wrong_issuer_and_audience`, `apikey_mode_end_to_end_over_the_router`,
and `rate_limit_keys_off_the_verified_principal_not_a_spoofed_header` (proving
a downstream consumer — SEC-203 — actually sees the verified identity, not the
raw header).

---

## SEC-102 `[P0]` — Remove the anonymous-default-tenant authorization bypass

**Problem.** `tenant_authorize` (`crates/wovyr-server/src/tenancy.rs:542-553`)
short-circuits `Ok(tenant)` — skipping `authorize()` entirely — whenever
`principal.is_empty() && tenant == DEFAULT_TENANT`, which is the state when no
headers are sent. Every sensitive route funnels through this: KMS crypto-shred
(`kms.rs`, `kms:admin`), secrets read/write (`secrets.rs`), audit read
(`audit.rs`). An unauthenticated caller with no headers can destroy a tenant's key
material. (PRD-003 R-1.2; closes PP-02.)

**Change.**
- Delete the bypass branch (`tenancy.rs:548-550`). All routes go through
  `ctx.authorize(scope)`.
- Preserve local-dev ergonomics behind an explicit opt-in: `WOVYR_ALLOW_ANONYMOUS=1`
  grants the anonymous default-tenant role set **only** when the listener is bound to
  loopback; refuse the flag (fail to boot, or ignore + warn loudly) on any
  non-loopback bind.
- Wire this to SEC-101's `disabled-loopback` auth mode so there is one code path for
  "no auth, dev only."

**Acceptance criteria.**
- With no `WOVYR_ALLOW_ANONYMOUS`: an unauthenticated request to any mutating/secret/
  KMS/audit route → `401`/`403`.
- With `WOVYR_ALLOW_ANONYMOUS=1` on a non-loopback bind: server refuses to start (or
  logs an error and does not grant anonymous access).
- Existing tenant-scoped tests updated to pass a credential.

**Files.** `crates/wovyr-server/src/tenancy.rs`, `lib.rs` (`serve` bind check).
**Size.** M. **Depends on:** SEC-101. **Blocks:** SEC-105.

**Status: Done, then further narrowed (RM-GA-P4/GA-003, 2026-07-09).** This
ticket's original fix made the bypass conditional on `AppState.anonymous_allowed`
(`auth::resolve_anonymous_allowed`, real-deployment default `false`, opt-in via
`WOVYR_ALLOW_ANONYMOUS=1`), with `serve()`'s `check_insecure_bind` refusing to
bind a non-loopback address when that flag is set. That was SEC-102's own
literal, intentional design — "gated, not open by default" — but
`compliance-mapping.md` §7 item 1 still flagged it as a residual finding worth
closing further at GA, since `WOVYR_ALLOW_ANONYMOUS=1` alone still granted an
anonymous caller *every* scope with zero grant. **That residual has now been
closed too**: the bypass branch in `tenant_authorize` is deleted outright.
`anonymous_allowed` today governs only whether `auth::authenticate`'s
`disabled-loopback` mode lets an unauthenticated request reach a handler at
all — RBAC downstream (`ctx.authorize(scope)`) is unconditional, and an empty
role set denies every scope exactly like a real principal with no memberships
would. Proven by
`anonymous_default_tenant_caller_reaches_kms_admin_only_up_to_the_auth_layer_now`
(flag on → `403`, not the old `200`/"destroyed"),
`anonymous_default_tenant_caller_is_denied_when_the_flag_is_off` (flag off →
`401`), and `refuses_anonymous_on_non_loopback_bind_only_when_flag_is_set`.
~10 wovyr-server tests that relied on the old permissive bypass (mostly in
`workflow_runner.rs`) were migrated to a real `WOVYR_PLATFORM_ADMINS` principal
rather than left passing on a now-false assumption. See
[A3-security-completion.md §3 item 3](A3-security-completion.md) for the
GA-completion tracking.

---

## SEC-103 `[P0]` — Gate plugin lifecycle routes with platform-admin RBAC

**Problem.** `install/enable/upgrade/trust/uninstall` handlers in
`crates/wovyr-server/src/plugins.rs` take only `Json(req)` — no `HeaderMap`, no
`tenant_authorize` call. An anonymous caller can trust their own key, install and
enable a WASM tool, and that tool then runs *inside every tenant's agent runs* with
tenant-scoped secrets injected. (PRD-003 R-1.3; closes PP-03.)

**Change.**
- Add `HeaderMap` to each plugin-mutation handler and call
  `tenant_authorize(&state, &headers, "plugins:admin")` (new scope) before acting.
- `plugins:admin` is a platform-admin-tier scope (mirror how `kms:admin` is
  defined/tested in `wovyr-tenancy`); add it to the privilege-ladder test.
- Attribute the action to the verified principal for the audit record (WS-8/R-8.4
  will consume this).

**Acceptance criteria.**
- Each of install/enable/disable/upgrade/rollback/trust/uninstall → `403` for a
  non-admin, `200/201` for a platform admin.
- A regression test asserts an anonymous caller cannot `trust` a publisher key.

**Files.** `crates/wovyr-server/src/plugins.rs`; `crates/wovyr-tenancy/src/` (scope
definition + ladder test). **Size.** M. **Depends on:** SEC-101. **Blocks:**
SEC-105.

**Status: Done.** Every plugin-mutation handler now takes `HeaderMap` and calls
`tenant_authorize(&state, &headers, "plugins:admin")` before acting. Proven by
`plugin_lifecycle_routes_require_plugins_admin`.

---

## SEC-104 `[P0]` — Gate marketplace moderation & publish routes with RBAC

**Problem.** `crates/wovyr-server/src/marketplace.rs` has **no** `tenant_authorize`
call anywhere; `approve_review`/`resolve_abuse_report` attribute the actor from a raw
header string (`actor_identity`, ~`marketplace.rs:427`) with no RBAC check. Anyone
can approve their own listing to "verified," delist a competitor, or publish. (PRD-003
R-1.3; closes PP-03.)

**Change.**
- Gate moderation routes (`approve_review`, `reject_review`, `resolve_abuse_report`,
  `dismiss_abuse_report`, `set_verified`) on `marketplace:moderate`.
- Gate `publish` on an authenticated publisher identity (a real principal, not an
  anonymous string); keep `search`/`get`/`download` public (or read-scoped) per
  product intent.
- Derive `actor_identity` from the verified principal, not the header.

**Acceptance criteria.**
- Moderation routes → `403` without `marketplace:moderate`.
- `publish` rejects an unauthenticated caller.
- Discovery/download behavior unchanged for legitimate read callers.

**Files.** `crates/wovyr-server/src/marketplace.rs`; `wovyr-tenancy` (scope). **Size.**
M. **Depends on:** SEC-101. **Blocks:** SEC-105.

**Status: Done.** Moderation routes now gate on `marketplace:moderate`;
`publish` requires an authenticated principal; `actor_identity` derives from
the verified principal. Proven by `moderation_routes_require_marketplace_moderate`
and `publish_requires_an_authenticated_principal`.

---

## SEC-105 `[P0]` — Negative-authorization test suite as a CI gate

**Problem.** No systematic proof that routes fail closed. Gaps (SEC-103/104) went
unnoticed precisely because nothing asserted "unauthenticated → denied" across the
route table. (PRD-003 R-1.4; closes PP-01/02/03 verification.)

**Change.**
- A table-driven integration test enumerating every mutating/secret/KMS/plugin/
  marketplace/audit route and asserting `401`/`403` for (a) no credential and (b) a
  valid-but-under-scoped credential.
- Run it in CI on every PR (add to `.github/workflows/ci.yml`).

**Acceptance criteria.**
- The suite covers 100% of mutating routes (a lint/count check prevents a new route
  from being added without an entry).
- CI fails if any listed route returns `2xx` for an unauthorized caller.

**Files.** `crates/wovyr-server/tests/authz_matrix.rs` (new); `.github/workflows/ci.yml`.
**Size.** M. **Depends on:** SEC-101, SEC-102, SEC-103, SEC-104.

**Status: Done.** `crates/wovyr-server/tests/authz_matrix.rs` exists and is
wired into `.github/workflows/ci.yml`, running on every PR.

---

# WS-2 — Transport & Resource Hardening

## SEC-201 `[P0]` — Request timeout, body-size limit, and concurrency cap

**Problem.** The router's only layer is `hardening::request_id`
(`crates/wovyr-server/src/lib.rs:560-603`); grep finds no `Timeout`/`body_limit`/
`ConcurrencyLimit`. `run_definition` holds the HTTP connection and a project
`RunPermit` for the whole agent loop — a wedged upstream holds both indefinitely.
(PRD-003 R-2.2; closes PP-05 in part.)

**Change.**
- Add `tower-http` (new workspace dep) layers: `TimeoutLayer`, `RequestBodyLimitLayer`
  (`DefaultBodyLimit`), and a `tower::limit::ConcurrencyLimitLayer` (or
  `GlobalConcurrencyLimitLayer`), all env-configurable
  (`WOVYR_HTTP_TIMEOUT_SECS`, `WOVYR_HTTP_MAX_BODY_BYTES`, `WOVYR_HTTP_MAX_CONCURRENCY`).
- Sensible defaults (e.g. 30s timeout, 1 MiB body, bounded concurrency); the agent
  run path may need a longer per-route timeout — apply a route-scoped override rather
  than a huge global.

**Acceptance criteria.**
- An oversized body → `413`; a slow request past the timeout → `408`/`504`; load past
  the concurrency cap sheds cleanly (no unbounded task growth).
- The run permit is released when the client-facing timeout fires.

**Files.** `Cargo.toml` (workspace dep `tower-http`), `crates/wovyr-server/src/lib.rs`.
**Size.** S. **Depends on:** none. *(Land early — cheap, high value.)*

**Status: Done.** `HttpLimits` (env-configurable) drives a `TimeoutLayer`,
`DefaultBodyLimit`, and a `ConcurrencyLimitLayer` in `router()`; the agent-run
routes get their own longer timeout tier. Proven by
`oversized_body_is_rejected_with_413`, `timeout_layer_converts_elapsed_into_408`,
`concurrency_limit_sheds_load_past_the_cap_then_recovers`, and
`timeout_drops_the_inner_future_releasing_raii_guards_held_across_it` (the run
permit is released when the timeout fires).

---

## SEC-202 `[P0]` — TLS termination or refuse insecure non-loopback bind

**Problem.** `serve()` (`crates/wovyr-server/src/lib.rs:635-643`) binds a plain
`TcpListener` — cleartext HTTP only. All traffic, including credentials (post
SEC-101) and secret responses, is plaintext. (PRD-003 R-2.1; closes PP-05 in part.)

**Change.**
- Add optional in-process TLS via a rustls acceptor (`axum-server` with the `tls-rustls`
  feature, or `tokio-rustls` directly). `rustls`/`ring` are already in the graph via
  `reqwest`.
- Config: `WOVYR_TLS_CERT`/`WOVYR_TLS_KEY` enable TLS. If neither is set **and** the
  bind address is non-loopback **and** `WOVYR_TLS_TERMINATED_UPSTREAM` is not declared,
  refuse to start.

**Acceptance criteria.**
- With cert/key: server serves HTTPS; a plaintext request is rejected.
- Non-loopback bind without TLS and without the upstream-termination flag → boot
  failure with a clear message.
- Loopback dev bind still works without TLS.

**Files.** `crates/wovyr-server/src/lib.rs`; `Cargo.toml` (TLS server dep);
`deployment/*` docs note. **Size.** M. **Depends on:** none.

**Status: Done.** `serve()` installs an in-process rustls acceptor
(`axum_server::bind_rustls`) when `WOVYR_TLS_CERT`/`WOVYR_TLS_KEY` are set;
`check_insecure_bind` refuses a non-loopback bind without TLS unless
`WOVYR_TLS_TERMINATED_UPSTREAM` declares a fronting proxy. Proven by
`tls_config_serves_https_end_to_end` and
`insecure_bind_is_refused_only_without_tls_and_without_upstream_termination_on_non_loopback`.

---

## SEC-203 `[P1]` — Per-principal and per-IP rate limiting

**Problem.** No throttling anywhere; combined with the destroy-key (PP-02) and
unmetered-run (PP-07/quota) vectors this is a trivial DoS. (PRD-003 R-2.3; closes
PP-05 in part.)

**Change.**
- Add a rate-limit layer keyed by verified principal (falling back to client IP for
  anonymous/public routes), e.g. `tower_governor` or a small token-bucket keyed map.
- Tighter buckets for expensive/sensitive routes (`agents:run`, KMS, secrets).

**Acceptance criteria.**
- Exceeding the bucket → `429` with a `Retry-After` header.
- Limits are per-principal (one noisy tenant cannot starve others) and configurable.

**Files.** `crates/wovyr-server/src/` (layer); `Cargo.toml`. **Size.** M.
**Depends on:** SEC-101 (principal keying).

**Status: Done.** `rate_limit.rs`'s dependency-free per-key token bucket backs
two tiers `router()` wires — `standard` and a tighter `sensitive` tier for
agent-run/KMS/secrets — keyed by the *verified* principal (client IP
fallback for anonymous callers), sitting after the auth layer so it can't be
bypassed by spoofing the header. Proven by
`standard_tier_rate_limit_returns_429_with_retry_after_then_isolates_by_principal`,
`rate_limit_keys_off_the_verified_principal_not_a_spoofed_header`, plus unit
tests for admission/refill/sweep/independent-keys.

---

## SEC-204 `[P1]` — CORS allow-list layer

**Problem.** No CORS layer exists; the dashboard only works behind Angular's dev
proxy or same-origin, and a built SPA from another origin fails preflight. (PRD-003
R-2.4; supports the WS-8 dashboard work.)

**Change.**
- Add `tower_http::cors::CorsLayer` with a configurable allow-list
  (`WOVYR_CORS_ALLOWED_ORIGINS`, default: none / same-origin), correct handling of
  credentialed requests and the custom `X-Wovyr-*` / `Idempotency-Key` headers.

**Acceptance criteria.**
- A configured origin passes preflight; an unlisted origin is refused.
- Default posture is same-origin only (no wildcard with credentials).

**Files.** `crates/wovyr-server/src/lib.rs`; `Cargo.toml`. **Size.** S. **Depends
on:** none. *(Coordinate with WS-8/R-8.5 dashboard login.)*

**Status: Done.** `cors_layer(&state.cors_allowed_origins)` (from
`WOVYR_CORS_ALLOWED_ORIGINS`) builds a `tower_http::cors::CorsLayer`
allow-list; no configured origins means no CORS headers at all (same-origin
only), not a wildcard. Proven by
`configured_origin_passes_preflight_but_unlisted_origin_gets_no_allow_header`
and `no_configured_origins_means_no_cors_headers_at_all`. WS-8/R-8.5's
dashboard login work (not yet done) is what will actually *use* this.

---

## SEC-205 `[P1]` — Bound and TTL-evict the idempotency store

**Problem.** `IdempotencyStore` is an unbounded in-memory `HashMap`
(`crates/wovyr-server/src/hardening.rs:88-94`, comment: "a TTL/eviction policy is a
later refinement"). A client generating fresh `Idempotency-Key`s grows server memory
forever; replay protection also vanishes on restart. (PRD-003 R-2.5; closes the
idempotency portion of PP-07.)

**Change.**
- Add TTL-based eviction and a max-entry bound (LRU or time-wheel). Time is read at
  the server boundary only, consistent with the determinism convention.
- Persistence across restart is covered by WS-4/R-4.4 (Phase 2) — this ticket only
  bounds memory; note the dependency.

**Acceptance criteria.**
- Entries expire after a configurable TTL; total entries are capped.
- A soak test with unique keys shows bounded memory.

**Files.** `crates/wovyr-server/src/hardening.rs`. **Size.** S. **Depends on:** none.

**Status: Done.** `IdempotencyStore` evicts by TTL and caps total entries
(FIFO eviction of the oldest when at capacity), both configurable. Later
extended from `agents:run` only to every mutating route (RM-GA-P4 API-703).
Proven by `entries_expire_after_the_configured_ttl`,
`total_entries_are_capped_evicting_the_oldest_first`, and
`soak_with_unique_keys_stays_within_the_entry_cap`.

---

# WS-3 — Safe-by-Default Tool Sandboxing

## SEC-301 `[P0]` — Do not register `shell` by default in server/hosted context

**Problem.** `ToolRegistry::with_builtins()`
(`crates/wovyr-tools/src/registry.rs:25-32`) registers `EchoTool`, `FsReadTool`,
`HttpGetTool`, **and `ShellTool`** unconditionally — arbitrary command execution as
the server user, contradicting the builtin doc comment claiming shell is "deferred."
(PRD-003 R-3.1; closes PP-04 in part.)

**Change.**
- Split the builtin set: a safe default (`with_builtins()` → echo, fs_read [confined,
  see SEC-302], http_get [guarded, see SEC-304]) and an explicit
  `with_shell()`/`with_privileged_builtins()` opt-in.
- The server registry construction path must not include `shell` unless an operator
  sets `WOVYR_ENABLE_SHELL_TOOL=1`.

**Acceptance criteria.**
- Default server agent runs have no `shell` tool available (a run requesting it fails
  closed with a clear error).
- Opt-in flag re-enables it; covered by a test.

**Files.** `crates/wovyr-tools/src/registry.rs`; server registry construction in
`crates/wovyr-server/src/lib.rs`. **Size.** S. **Depends on:** none.

**Status: Done.** `with_builtins()` registers only echo/fs_read/http_get;
`shell` requires the separate, explicit `with_shell()`/
`with_privileged_builtins()`. The server only adds it when an operator sets
`WOVYR_ENABLE_SHELL_TOOL=1`. Proven by `shell_tool_opt_in_env_var_re_enables_it`.

---

## SEC-302 `[P0]` — Confine `fs_read` to an allow-listed workspace root

**Problem.** `FsReadTool::execute` (`crates/wovyr-tools/src/builtin.rs:83-96`) calls
`tokio::fs::read_to_string(path)` on any caller-supplied path — no confinement. An
agent can read `~/.wovyr/kms/root.key`, `~/.wovyr/secrets/secrets.json`, `/etc/passwd`.
Reading the KMS root key defeats the entire at-rest encryption design. (PRD-003 R-3.2;
closes PP-04 — the highest-impact item in this workstream.)

**Change.**
- Introduce a workspace-root confinement: `fs_read` resolves the requested path
  against a configured root (from `ToolContext`, e.g. the run's workspace), canonicalizes
  it, and rejects any path escaping the root (symlink-aware — canonicalize then verify
  prefix).
- The confinement root must **never** include `~/.wovyr` or other platform state.

**Acceptance criteria.**
- Reading a file inside the workspace succeeds; `../`, absolute paths outside the
  root, and symlink escapes → `PermissionDenied`.
- A regression test explicitly asserts `~/.wovyr/kms/root.key` is unreadable.

**Files.** `crates/wovyr-tools/src/builtin.rs`, `crates/wovyr-tools/src/tool.rs`
(`ToolContext` workspace-root field if not present). **Size.** M. **Depends on:**
none.

**Status: Done.** `fs_read` resolves the requested path against the run's
workspace root, canonicalizes, and rejects any escape (symlink-aware).
Proven by `fs_read_allows_a_path_inside_the_workspace_root`,
`fs_read_denies_dot_dot_traversal_out_of_the_root`,
`fs_read_denies_an_absolute_path_outside_the_root`,
`fs_read_denies_a_symlink_that_escapes_the_root`, and the explicit regression
test `fs_read_cannot_reach_the_kms_root_key`.

---

## SEC-303 `[P0]` — Default permission grants to deny (empty) in hosted context

**Problem.** `run_agent` sets `granted_permissions: def.spec.permissions.clone()`
(`crates/wovyr-agent/src/runtime.rs:288`), `permissions` is `Option`
(`definition.rs`), and the registry treats `None` as unrestricted. So any manifest
without a `permissions:` block can call every tool freely. (PRD-003 R-3.3; closes
PP-04 in part.)

**Change.**
- In a hosted/server context, `None` grants MUST mean **deny-all**, not unrestricted;
  a manifest must explicitly list the tool permissions it needs.
- Keep an escape hatch for trusted first-party/local use (`WOVYR_UNRESTRICTED_TOOLS=1`
  or a `TrustClass::FirstParty` context), so local CLI ergonomics are preserved while
  the network-facing default is fail-closed.
- Thread the decision through `ToolContext.granted_permissions` so `check_permissions`
  enforces it.

**Acceptance criteria.**
- A manifest with no `permissions:` block, run in server context, cannot invoke any
  permissioned tool (`PermissionDenied`).
- An explicit allow-list works; the first-party escape hatch works and is tested.

**Files.** `crates/wovyr-agent/src/runtime.rs`, `crates/wovyr-tools/src/tool.rs`/`registry.rs`.
**Size.** M. **Depends on:** none. **Blocks:** SEC-305 (shares run-context plumbing).

**Status: Done.** A hosted run (`RunOptions::with_hosted(true)`, set on every
server-submitted run) with no manifest `permissions:` block now grants
**nothing** (`Some(vec![])`), not unrestricted access; `WOVYR_UNRESTRICTED_TOOLS=1`
is the documented escape hatch for trusted first-party/local use. Enforced by
`ToolRegistry::check_permissions` against `ToolContext.granted_permissions`.

---

## SEC-304 `[P1]` — SSRF guard in `http_get`

**Problem.** `HttpGetTool` (`crates/wovyr-tools/src/builtin.rs:156-198`) accepts any
`http(s)` URL with no allow-list and no private-range block; the only real egress
enforcement (`egress_lockdown.rs`) is on the Linux/Docker container path, not the
native/default path the builtins use. An agent can reach `169.254.169.254`
(cloud metadata), internal services, and localhost admin ports. (PRD-003 R-3.4;
closes PP-04 in part.)

**Change.**
- Resolve the target host and reject link-local (`169.254.0.0/16`, `fe80::/10`),
  loopback, private (`10/8`, `172.16/12`, `192.168/16`, `fc00::/7`), and
  metadata addresses **before** connecting; guard against DNS-rebinding by pinning the
  resolved IP for the request.
- Support a per-tenant egress allow-list from `ToolContext`; default-deny to internal
  ranges regardless.

**Acceptance criteria.**
- Requests to metadata/loopback/private ranges → `PermissionDenied`, on all platforms.
- A DNS name resolving to a private IP is blocked.
- Allow-listed public hosts still work.

**Files.** `crates/wovyr-tools/src/builtin.rs`; a small IP-classification helper.
**Size.** M. **Depends on:** none.

**Status: Done.** `HttpGetTool` resolves the target host and rejects
link-local/loopback/private/metadata ranges before connecting, pinning the
resolved IP for the actual request to defeat DNS-rebinding. Proven by the
`builtin.rs` SEC-304 test block.

---

## SEC-305 `[P1]` — Drive sandbox selection from a real `TrustClass` on the run path

**Problem.** `ShellTool` hardcodes `TrustClass::FirstParty`
(`crates/wovyr-tools/src/builtin.rs`), and nothing maps an untrusted agent/plugin to
`TrustClass::Untrusted`/gVisor at runtime — the `TrustClass::floor` logic in
`sandbox.rs` exists but is never driven from the agent run path. (PRD-003 R-3.5;
closes PP-04 in part.)

**Change.**
- Thread a `TrustClass` derived from provenance (first-party manifest vs. installed
  plugin vs. untrusted/marketplace) through `ToolContext` into `select_backend`, so
  untrusted work cannot select the native backend and is floored to
  container/gVisor (where available; fail-closed where not).

**Acceptance criteria.**
- An untrusted-provenance run cannot execute on `NativeSandbox`; selection honors the
  trust floor.
- Backend-selection unit tests cover first-party vs. untrusted provenance.

**Files.** `crates/wovyr-agent/src/runtime.rs`, `crates/wovyr-tools/src/sandbox.rs`,
`crates/wovyr-tools/src/tool.rs`. **Size.** M. **Depends on:** SEC-303.

**Status: Done.** `ToolSpec.trust_class` (default `TrustClass::FirstParty`)
flows into `select_backend`, flooring an untrusted/verified-but-not-first-party
run away from `NativeSandbox`. Proven by the `builtin.rs` SEC-305 test block.

---

# Rollup

| Ticket | Title | Size | Priority | Depends on |
|--------|-------|------|----------|------------|
| SEC-101 | Credential verification layer — **Done** | L | P0 | — |
| SEC-102 | Remove anonymous-default bypass — **Done** | M | P0 | SEC-101 |
| SEC-103 | Plugin routes RBAC — **Done** | M | P0 | SEC-101 |
| SEC-104 | Marketplace routes RBAC — **Done** | M | P0 | SEC-101 |
| SEC-105 | Negative-auth CI gate — **Done** | M | P0 | 101,102,103,104 |
| SEC-201 | Timeout/body/concurrency — **Done** | S | P0 | — |
| SEC-202 | TLS or refuse insecure bind — **Done** | M | P0 | — |
| SEC-203 | Rate limiting — **Done** | M | P1 | SEC-101 |
| SEC-204 | CORS allow-list — **Done** | S | P1 | — |
| SEC-205 | Idempotency TTL/bound — **Done** | S | P1 | — |
| SEC-301 | No default shell — **Done** | S | P0 | — |
| SEC-302 | fs_read confinement — **Done** | M | P0 | — |
| SEC-303 | Deny-by-default grants — **Done** | M | P0 | — |
| SEC-304 | http_get SSRF guard — **Done** | M | P1 | — |
| SEC-305 | TrustClass on run path — **Done** | M | P1 | SEC-303 |

**Rough total:** 1 L + 9 M + 4 S ≈ 8–10 engineer-weeks, parallelizable to ~3–4
calendar weeks across 2–3 engineers. **Phase-1 exit — met:** PRD-003 §11 item 1
(no unauthenticated mutation, TLS+limits, safe default tools) is satisfied by
the shipping binary; see each ticket's Status note above for the file/test
evidence.

---

# Related

- [PRD-003](../../01-product/prd-ga-hardening.md) — parent PRD (WS-1/2/3, §10 phasing)
- [ADR-0010](../../17-adr/ADR-0010-ga-deployment-topology.md) — GA topology decision
- [`13-security/index.md`](../../13-security/index.md) · [`07-tool-runtime/security-isolation.md`](../../07-tool-runtime/security-isolation.md) (sandbox floors, egress lockdown)

---

# Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.1.0 | 2026-07-07 | **Documentation-drift correction, not new work:** all 15 tickets were already shipped (commits `8a9e4cb`, `64ab69d`, `8a48eb0`, `da5f0dd`, `8f3cb0e`, `9ca9228`, all dated before this doc's last update) but this file was never updated to say so — it still read "Ready for grooming" with zero Done markers. Added a Status note with file/test evidence to every ticket, marked the rollup table, and corrected the header. Found while doing a broader project status review; the same review found `CLAUDE.md` and `docs/09-api/overview.md` had the identical staleness in the other direction (still describing auth as unverified headers) — fixed in the same pass |
| 1.0.0 | 2026-07-06 | Initial Phase-1 (security floor) ticket breakdown: 15 tickets across WS-1/2/3 with dependencies, acceptance criteria, file targets, and sizing |
