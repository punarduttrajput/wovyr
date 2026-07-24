<!--
File: docs/01-product/prd-audit-remediation.md
Document ID: PRD-007
-->

# PRD: Audit Remediation & Truth Reconciliation

**Document ID:** PRD-007
**File Path:** `docs/01-product/prd-audit-remediation.md`
**Version:** 1.0.0
**Status:** Draft
**Owner:** Product / Founder
**Last Updated:** 2026-07-23

---

# 1. Purpose

On 2026-07-23 the product went through a four-lens audit — QA/reliability, AI
engineering, security, and codebase-health/strategy — reading the actual code
against the claims in `README.md`, `CLAUDE.md`, and `DISTRIBUTION.md` rather than
trusting the self-reported status. The audit's headline finding: **the
engineering is genuinely high quality and the docs culture is unusually honest,
but the scope is unsustainable for a single maintainer, the differentiated "AI"
is a thin slice of the code, and several headline claims — precisely the ones
that anchor the only monetizable wedge (the regulated/self-hosted buyer) — are
materially overstated against what the code actually does.**

This PRD scopes the remediation: **fix the concrete correctness and security
defects a real review would find on day one, reconcile the product's claims with
its reality, and put a sustainability boundary around the scope** — so the
regulated-buyer positioning becomes honest rather than aspirational, and the
maintenance surface stops growing faster than one person can own it.

This is a **truth-and-hardening** milestone, not a feature milestone. Its
guiding rule, borrowed from `DISTRIBUTION.md`: *an honest narrow claim beats an
impressive broad one that fails audit.*

---

# 2. Problem Statement

The audit produced concrete, file:line-backed findings. Grouped by lens:

## 2.1 Security — exploitable gaps in the load-bearing claims

1. **SSRF via HTTP redirect.** `builtin.rs`'s `pinned_client` builds a
   DNS-pinned client but does not disable reqwest's default redirect-following,
   and the SSRF guard validates only the *original* host. A fetch to an
   attacker URL that returns `302 → http://169.254.169.254/...` reaches the
   cloud-metadata endpoint unguarded. Applies to both `http_get` and the MCP
   `Http` transport.
2. **Cross-tenant authorization gap.** `apex-server`'s `tenancy::context()`
   grants an organization-scoped role whenever the request is project-less, via
   a `... || project.is_none()` clause. Any authenticated principal holding an
   org role in *any* tenant can set `X-Apex-Tenant: <other>` and
   enumerate/create organizations in a tenant they have no membership in.
3. **"Tamper-evident audit log" is overstated.** The chain is an *unkeyed*
   SHA-256 with no external anchor. An actor with write access to `audit.jsonl`
   can rewrite entries and recompute the chain, and **tail-truncation is
   undetectable** (no persisted head/high-water-mark). This directly undermines
   the compliance/EU-AI-Act logging pitch.
4. **"Sandboxed tools" is false on the default, cross-platform path.**
   First-party `shell`/`code_execute` runs on `NativeSandbox` — resource limits
   only, no filesystem or network confinement. The real isolation
   (`--network none`, iptables/nsenter egress lockdown) engages only for
   non-first-party trust classes *and only on Linux+Docker*. On Windows/macOS
   there is no tool egress control at any trust class.
5. **KMS ephemeral-fallback data-loss footgun.** The server mints an in-memory
   root key if `HOME` is unresolvable (e.g. a container with no persistent
   volume), silently producing data that is unrecoverable on restart, rather
   than failing closed.
6. **SSRF IP blocklist misses encapsulated ranges** — 6to4, NAT64, CGNAT
   (`100.64.0.0/10`) can route to internal/metadata targets on hosts with such
   routing.

## 2.2 AI correctness — features that don't work in shipped configurations

7. **An Anthropic-only deployment breaks all memory/RAG.** `AnthropicProvider`
   implements no `embed`, and `Gateway::embed` is a no-failover pass-through to
   the first provider, so every `remember`/`query` call errors on a deployment
   configured with only `ANTHROPIC_API_KEY` — a first-class, documented config.
8. **Unbounded caches.** The gateway's exact cache (`HashMap`) only ever
   inserts (TTL checked on lookup, never swept); the semantic cache is a `Vec`
   with a linear cosine scan over every entry ever stored. Both grow without
   bound in a long-running server — a real memory-leak + latency-growth defect.
9. **The advanced AI features are inert in the default config.** Semantic
   cache, hybrid *vector* RAG, and MMR diversification all depend on a real
   embedding provider; the zero-config default is the mock provider (non-semantic
   hash embeddings), so hybrid RRF blends real keyword ranks with random vector
   noise — worse precision than keyword-only.
10. **Multimodal tokens are ignored** in context budgeting (an image part
    counts as ~4 tokens), defeating the compactor for exactly the payloads that
    threaten the window; and the OpenAI adapter sends `max_tokens`/`temperature`
    unconditionally, hard-failing on o1/o3-class reasoning models.
11. **Workflow `${...}` data references are unvalidated against DAG edges** and
    silently resolve to `null`, turning a whole class of authoring mistakes into
    silent wrong-data instead of a load-time error.

## 2.3 QA — "proven" claims with no reproducible guarantee

12. **Acceptance tests for "proven"/"closed" features run in no CI job** — the
    SRV-307 fleet-shared-concurrency test, the sandbox-backend suite, and the
    egress-lockdown adversarial tests are all capability-gated *and* invoked by
    no CI target.
13. **Coverage is measured but never gated** — no floor; a PR may drop coverage
    to any level and pass.
14. **~11 "verified live" claims have no reproducible test** — S3 backup
    (admitted unvalidated against a live endpoint), Postgres TLS (CI uses
    plaintext), cross-process CLI↔server KMS, browser e2e. Highest-confidence
    *wording*, weakest ongoing guarantee.

## 2.4 Strategy / sustainability — scope outrunning one maintainer

15. **~20 product-grade subsystems, 23 packages, ~79k Rust LOC, bus factor 1,**
    produced in ~26 days (~4,600 LOC/day) — a velocity only reachable via heavy
    AI generation, faster than any one human can durably own, across a security
    core (KMS, audit, sandbox) where a subtle flaw is a CVE.
16. **Version/maturity contradiction** — the artifact ships `0.3.0`
    (`Cargo.toml`) while the roadmap narrates "v1.0 GA / v1.1 / v1.2 / v1.3
    shipped." A buyer sees pre-1.0; the story says enterprise-GA.
17. **37 backend implementations × 5 sandbox tiers × ~24 cargo features** — an
    untestable matrix; large swaths exercise only in a service-container CI job,
    not on any developer machine.
18. **Hand-rolled security-critical code** — a 694-LOC SigV4 signer (admitted
    unvalidated against a live endpoint), a hand-rolled Postgres pool, and the
    egress firewall — the kind of code easiest to get subtly wrong and hardest
    for one person to keep correct.
19. **Built-but-unwired features** — reranking, MMR, guardrails, the prompt
    registry, multimodal parts, and structured output are all "engine-level
    only, not surfaced in server/CLI," i.e. maintained code delivering zero
    user value today.

---

# 3. Baseline: what already exists to build on

| Existing asset | Role in this PRD |
|---|---|
| `http_get`'s SEC-304 `resolve_and_guard` + DNS-pinned client | The guard SEC-401 must extend to redirects and SEC-406 to more ranges — fix in one shared place; MCP `Http` transport inherits it |
| `apex-server`'s `tenancy::context()` + the `tenant_authorize` default-deny path | The single function SEC-402 corrects; the SEC-105 authz-matrix CI job is where its regression test belongs |
| `apex-audit`'s hash chain + `fsync` durability + concurrent-append fix | The *consistency* foundation SEC-403 upgrades to *tamper-resistance* (keyed MAC + head anchor), reusing the existing `AuditSink`/`verify` shape |
| The `apex-kms` `Kms` trait + `root::from_env`/`from_file` | SEC-405 only changes the *fallback* branch to fail-closed; the crypto is untouched |
| `apex-provider`'s `AIProvider::embed` default + `Gateway` provider list | AIC-301 adds a distinct embedding-provider resolution or a fail-loud config check at the trait/gateway boundary |
| The gateway's exact + semantic cache stores | AIC-302 bounds them (TTL sweep / LRU / capped scan) behind the existing `SemanticCacheStore` trait — no API change |
| `apex-provider::tokenizer`'s `TokenCounter` | AIC-304 extends `count_message` to account for `parts`, behind the same trait |
| `apex-workflow`'s `Definition::from_yaml` validate-on-load + `resolve_template` | WFL-309 adds a load-time reference/edge cross-check to the existing validation pass |
| CI's `services-integration` job (real Postgres/Qdrant/Redis containers, fails-on-skip) | QA-401 adds the missing gated targets to this exact job; QA-403 adds a MinIO service for the S3 path |
| `cargo llvm-cov` in the `coverage` job | QA-402 adds a `--fail-under` threshold to what already runs |
| The workspace `version` field + `CHANGELOG.md` (DX-101) | STR-501 reconciles the version number with the narrative in one lockstep bump |

---

# 4. Goals & Non-Goals

## 4.1 Goals

- **G1 — Close the day-one findings.** No exploitable SSRF, no cross-tenant
  authorization escape, no silent KMS data loss — verified by regression tests,
  not asserted.
- **G2 — Make the claims true or change the claims.** Every headline security
  and AI claim in `README.md`/`CLAUDE.md`/`DISTRIBUTION.md` either matches the
  code, or is qualified to match it. "Tamper-evident" and "sandboxed" in
  particular must be either upgraded or precisely scoped.
- **G3 — The shipped default config must work.** Memory/RAG must not silently
  break on Anthropic-only; the default retrieval path must not be worse than
  keyword-only; caches must not leak.
- **G4 — "Proven" must mean "proven in CI."** Every feature CLAUDE.md calls
  proven/closed has a test that actually executes in a CI job, or the claim is
  downgraded to match its real guarantee.
- **G5 — Draw a sustainability line.** Freeze the subsystem count; label the
  heavy/unwired backends experimental; replace or validate the hand-rolled
  security code — so maintenance surface stops outrunning one owner.

## 4.2 Non-Goals

- **A ground-up rewrite or de-scoping of shipped, working subsystems.** The
  workflow engine, gateway resilience, KMS crypto, and auth are sound; this PRD
  hardens and reconciles, it does not rebuild them.
- **New product surface.** No new user-facing feature is in scope; wiring an
  *already-built* feature (STR-505) is explicitly a decision to *wire or cut*,
  not to expand.
- **Multi-maintainer / hiring / funding decisions.** Bus-factor-1 (finding 15)
  is surfaced as a documented strategic risk (STR-503's rationale); solving it
  organizationally is outside an engineering PRD.
- **Distributed multi-node scale-out.** Still gated on demand (PRD-004 §7),
  untouched here.
- **Sandboxing the native tool path on Windows/macOS to Linux parity.** SEC-404
  adds a confinement *floor* and honest scoping, not a full cross-platform
  container/egress equivalent (that remains a documented platform gap).

---

# 5. Personas & Use Cases

## 5.1 Personas

- **P1 — Regulated buyer's security reviewer.** The persona the whole
  positioning targets; runs a pentest and a claims-vs-reality check. Findings
  1–6 and 12–14 are what they surface.
- **P2 — Self-hosting operator.** Configures the appliance (Anthropic-only, a
  container with no persistent volume, Windows) and expects the advertised
  behavior. Findings 4, 5, 7 bite them silently.
- **P3 — Agent/workflow author.** Hits findings 9, 10, 11 as "why is retrieval
  bad / why did my model 400 / why is my activity input null" with no error.
- **P4 — The maintainer (you).** Findings 15–19: whether the surface is
  ownable at all.

## 5.2 Canonical use cases

- **UC1 — Pentest survives day one.** A reviewer attempts the redirect-SSRF and
  cross-tenant-org escalation; both are refused with a regression test proving
  it, not a manual spot-check.
- **UC2 — Anthropic-only appliance works.** An operator sets only
  `ANTHROPIC_API_KEY`, writes and queries memory, and it either works (via a
  configured embedding provider) or fails at config time with a clear message —
  never a per-call runtime error deep in a run.
- **UC3 — The claims match.** A reviewer reads "tamper-evident audit log" and
  "sandboxed tools," checks the code, and finds the claim is either implemented
  as stated or scoped to exactly where it holds.
- **UC4 — A long-running server is stable.** The gateway serves a week of
  traffic without unbounded memory growth or per-request latency creep from the
  caches.
- **UC5 — "Proven" is reproducible.** Every claim of "proven"/"verified live"
  maps to a CI job a reviewer can re-run, or is worded down to its real
  guarantee.

---

# 6. Workstreams & Requirements

Requirement IDs are stable and referenced by the [v1.4 roadmap
tickets](../18-roadmap/v1.4-audit-remediation.md). "Fail-closed" carries the
standing meaning: an error or unvalidated state never degrades into a
silently-broader grant or a silent wrong answer.

## WS-SEC — Security remediation & trust integrity — SEC-4xx

- **SEC-401** Disable automatic redirect-following in the DNS-pinned client and
  re-run the SSRF guard on every redirect hop (or reject redirects outright for
  tool fetches). Covers both `http_get` and the MCP `Http` transport.
- **SEC-402** Remove the `|| project.is_none()` org-role grant in
  `tenancy::context()`; an org-scoped role must require membership in the target
  org for org-level operations too. Add a regression test to the SEC-105 authz
  matrix asserting a tenant-A member is 403'd on org-level routes with
  `X-Apex-Tenant: B`.
- **SEC-403** Upgrade audit-log tamper-evidence to tamper-*resistance*: a keyed
  MAC (HMAC via a key held outside the log file, sourced like the KMS root key)
  over each entry, plus a persisted monotonic head/high-water-mark so tail
  truncation is detectable. `verify()` must fail on a rewritten *or* truncated
  chain. Optional external-anchor hook documented for the compliance tier.
- **SEC-404** Sandbox-claim reconciliation: (a) add a filesystem/network
  confinement *floor* to the native path for `shell`/`code_execute` where the
  host supports it, and (b) where it cannot (Windows/macOS, first-party native),
  make "sandboxed" an *accurate, scoped* claim in code docs and marketing —
  never an unqualified one. Fail-closed refusal + a clear message is acceptable
  where isolation is unavailable; a silent unsandboxed run is not.
- **SEC-405** KMS fail-closed on missing durable key material: refuse to start
  when neither `APEX_KMS_ROOT_KEY` nor a persistent, writable key file is
  available, instead of minting an ephemeral in-memory root key that loses all
  sealed data on restart.
- **SEC-406** Extend the SSRF IP blocklist to encapsulated/CGNAT ranges (6to4
  `2002::/16` wrapping metadata, NAT64 `64:ff9b::/96`, IPv4-compatible `::/96`,
  CGNAT `100.64.0.0/10`), in the same shared guard SEC-401 hardens.

## WS-AIC — AI-core correctness — AIC-3xx / WFL-309

- **AIC-301** Embedding-provider resolution: either implement an Anthropic-side
  embedding path (e.g. Voyage) or allow a *distinct* embedding provider in the
  gateway, and — regardless — make a deployment that cannot embed fail **loudly
  at config/startup time** if a memory/RAG feature is enabled, never per-call
  deep in a run.
- **AIC-302** Bound both caches: TTL sweep + a size cap (LRU eviction) on the
  exact cache; a bounded entry count and a non-linear (or capped) lookup on the
  semantic cache. Behind the existing store traits — no API change.
- **AIC-303** Default-config retrieval quality: when the resolved provider
  cannot produce semantic embeddings (the mock default), default the retrieval
  strategy to keyword/BM25 rather than a hybrid that blends real keyword ranks
  with random vector noise. Document the switch; hybrid re-enables automatically
  with a real embedding provider.
- **AIC-304** Multimodal-aware token counting: `TokenCounter::count_message`
  must account for `msg.parts` (image/audio) so context compaction reflects the
  real prompt cost.
- **AIC-305** OpenAI reasoning-model compatibility: send
  `max_completion_tokens` (not `max_tokens`) and omit non-1.0 `temperature` for
  o1/o3-class models, driven by a model capability check rather than a hard
  400.
- **WFL-309** Validate `${...}` data references against DAG edges at
  `Definition::from_yaml` time: an activity referencing another's output must
  have a corresponding edge (so it cannot batch/schedule before its source),
  and an unresolved reference is a load-time error, not a silent `null`.

## WS-QA — Verification truth — QA-4xx

- **QA-401** Wire the "proven"-but-unrun tests into CI: add the
  `tenancy::redis_tests` (SRV-307), `sandbox_backends`, and egress-lockdown
  targets to the `services-integration` job's explicit target list (which
  already fails-on-skip), so a regression in shared-concurrency, sandbox, or
  egress can no longer ship green.
- **QA-402** Add a coverage floor: a `--fail-under` threshold to the existing
  `cargo llvm-cov` job, set at (or just below) today's measured level so
  coverage cannot silently regress.
- **QA-403** Make the "verified live" claims reproducible or reclassify them:
  add a MinIO service container for the S3 SigV4 round trip, a TLS-enabled
  Postgres leg for the `sslmode=require` path, and a cross-process CLI↔server
  KMS test; for any claim that genuinely cannot be automated here, downgrade its
  wording from "verified live" to an honest "manually spot-checked, not CI-gated."

## WS-STR — Strategy, truth & sustainability — STR-5xx

- **STR-501** Version reconciliation: bump the workspace `version` to match the
  milestone narrative (or restate the narrative to match `0.x`), in lockstep
  across every manifest + `CHANGELOG.md` + README badge + `/healthz`, per the
  DX-101 process. One source of truth for "how mature is this."
- **STR-502** Claim-honesty pass: a single sweep of `README.md`, `CLAUDE.md`,
  and `DISTRIBUTION.md` reconciling every security/AI claim with the post-SEC/
  AIC reality — "tamper-evident" and "sandboxed" scoped precisely, the
  Anthropic/embedding and default-RAG caveats stated, the hand-rolled/unvalidated
  code flagged. Consolidate the three divergent positioning one-liners into one.
- **STR-503** Subsystem freeze + experimental labeling: no new subsystem without
  an explicit decision; label the heavy/unwired or infra-dependent backends
  (Firecracker/microVM, mistral.rs, Qdrant/Postgres tiers, the plugin
  marketplace) as **experimental** in docs and (where a cargo feature exists) in
  the feature's own doc comment, so the supported core is unambiguous. Records
  bus-factor-1 as an accepted, documented strategic risk.
- **STR-504** De-risk the hand-rolled security code: replace the hand-rolled
  SigV4 signer with a vendored, maintained crate **or** gate the S3 path behind
  the QA-403 MinIO round trip and an explicit "experimental" label until it is
  validated against a real endpoint; likewise document the hand-rolled Postgres
  pool's tested envelope.
- **STR-505** Wire-or-cut the built-but-unwired features: for each of reranking,
  MMR, guardrails, the prompt registry, multimodal parts, and structured output,
  make an explicit per-feature decision — surface it (server/CLI) or mark it
  experimental/removed — so no code is maintained solely to sit unreachable.

---

# 7. Phasing

Detailed tickets live in the [v1.4 roadmap](../18-roadmap/v1.4-audit-remediation.md).

| Phase | Theme | Requirements | Exit criterion |
|---|---|---|---|
| **P1 — Stop the bleeding** | Critical, small, high-value | SEC-401, SEC-402, SEC-405, SEC-406, AIC-301, AIC-302 | A pentest's day-one SSRF and cross-tenant escalations are refused with regression tests; an Anthropic-only appliance fails loud at config time, not per-call; the server runs a week without cache growth; the KMS refuses to start rather than lose data |
| **P2 — Make claims true** | Trust integrity & honesty | SEC-403, SEC-404, STR-501, STR-502, QA-401, QA-402, QA-403 | `verify()` catches a truncated audit chain; "tamper-evident"/"sandboxed" match the code or are scoped; the version and the narrative agree; every "proven" claim runs in CI or is downgraded |
| **P3 — Quality & sustainability** | AI polish + scope line | AIC-303, AIC-304, AIC-305, WFL-309, STR-503, STR-504, STR-505 | Default retrieval ≥ keyword-only; multimodal budgeting real; reasoning models don't 400; workflow ref mistakes fail at load; the supported subsystem set is frozen and labeled; hand-rolled security code is vendored or fenced; every built feature is wired or explicitly experimental |

**Sequencing note.** P1 first, and within it SEC-401/SEC-402 first of all — they
are small and they are the findings that end a security review. AIC-301 is P1
because an Anthropic-only appliance is a documented first-run configuration that
is currently broken.

---

# 8. Success Metrics

- **Zero surviving day-one findings:** the redirect-SSRF, cross-tenant-org, and
  encapsulated-range findings each have a regression test that fails against the
  pre-fix code and passes after (the "proven, not asserted" bar).
- **Claim/reality parity, measured:** a checklist of every security/AI claim in
  the three top-level docs, each marked matches-code or scoped-to-match, with
  zero unqualified overstatements remaining.
- **Default config works:** an Anthropic-only run either succeeds at memory/RAG
  or fails at startup with a clear message — never a per-call error (CI-gated);
  default retrieval precision on the audit's own fixtures is ≥ keyword-only.
- **"Proven" == CI:** every feature the docs call proven/closed executes in a
  named CI job; the coverage floor is enforced.
- **Scope frozen:** subsystem count does not increase during this milestone;
  every heavy/unwired backend is labeled experimental; the SigV4 path is either
  vendored or fenced-and-labeled.

---

# 9. Acceptance Narrative

> A regulated buyer's security reviewer runs their standard pass. They point a
> tool fetch at a server that 302-redirects to the cloud-metadata IP — refused,
> and there's a test proving it. They authenticate as an org-admin of tenant A
> and try to list tenant B's organizations — 403, with a matrix test proving it.
> They read "tamper-evident audit log," inspect `verify()`, truncate the tail of
> `audit.jsonl`, and `verify()` reports the tampering. They read "sandboxed
> tools" and find the claim scoped precisely to where it holds, with the native
> path's limits stated plainly. They deploy Anthropic-only; it refuses to start
> a memory-enabled agent with a clear "no embedding provider configured" message
> rather than failing mid-run. They check the version — it says one coherent
> thing. Nothing they find contradicts what they were told. The pilot conversation
> continues.

Every clause maps to a requirement above; this is the acceptance test first,
the security-review script second.

---

# 10. Risks & Mitigations

| Risk | Mitigation |
|---|---|
| SEC-403's keyed MAC needs a key held outside the log; a badly-sourced key just moves the problem | Source it the same way as the KMS root key (`APEX_KMS_ROOT_KEY`/escrowed file), reuse the SEC-405 fail-closed-on-missing-key stance |
| SEC-404's native confinement floor can't reach Linux+Docker parity on Windows/macOS | Explicitly scope the claim rather than over-promise; fail-closed refusal where isolation is unavailable is an accepted outcome, documented as a platform gap |
| AIC-301 "fail loud at config time" could break an existing deployment that relied on the silent-error path | It is already broken (errors per-call); moving the failure to startup is strictly more honest, and is a one-release "observe then enforce" rollout like PRV-101 |
| STR-503's "experimental" labels read as walking back shipped features | Framed as honest maturity signaling, not removal; the code stays and keeps its tests — the label sets buyer expectations correctly |
| The remediation itself expands scope (irony) | Every ticket is a fix, a reconciliation, or a *reduction* (label/cut); STR-503 forbids new subsystems for the milestone's duration |
| Fixing claims dents the impressive-sounding pitch | The wedge is regulated buyers who *audit*; an honest scoped claim that survives review is worth more than a broad one that fails it (the PRD's guiding rule) |

---

# 11. Relationship to Other Docs

- [PRD-003 (GA hardening)](prd-ga-hardening.md) and
  [PRD-004 (AI platform maturity)](prd-ai-platform-maturity.md) — the milestones
  whose claims this PRD reconciles; several findings are regressions or
  overstatements against tickets those PRDs marked done.
- [DISTRIBUTION.md](../../DISTRIBUTION.md) — the go-to-market plan whose
  positioning line this PRD makes defensible (STR-502) and whose "≤20% on code"
  mandate this milestone deliberately respects (it is remediation, not features).
- [v1.4 roadmap](../18-roadmap/v1.4-audit-remediation.md) — the phased tickets
  executing this PRD.
- `CLAUDE.md` — the self-reported status doc STR-502 reconciles with the code.

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-23 | Initial PRD: remediation of the 2026-07-23 four-lens audit — security day-one fixes (SEC-4xx), AI-core correctness (AIC-3xx/WFL-309), verification truth (QA-4xx), and strategy/sustainability reconciliation (STR-5xx). A truth-and-hardening milestone, not a feature milestone |
