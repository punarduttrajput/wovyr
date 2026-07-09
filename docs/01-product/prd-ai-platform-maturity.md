<!--
File: docs/01-product/prd-ai-platform-maturity.md
Document ID: PRD-004
-->

# PRD: AI Platform Maturity & Production Readiness

**Document ID:** PRD-004
**File Path:** `docs/01-product/prd-ai-platform-maturity.md`
**Version:** 1.0.0
**Status:** Draft — planning input, not a commitment
**Owner:** Product / AI Engineering
**Last Updated:** 2026-07-09

---

# 1. Purpose

[PRD-003](prd-ga-hardening.md) closed the *deployed-vs-designed* gap that made the
GA appliance defensible: authentication, crash-safety, an execution driver, a
contract-tested API. This PRD closes the next gap — the **capability-vs-credible-AI-product**
gap — surfaced by a 2026-07-09 full-project engineering audit (five parallel
deep-dives: AI core, workflow+server, dashboard UI, tools/plugins/sandbox, and
DX/CI/deployment/docs).

Where PRD-003 asked *"is the platform safe and honest?"*, this PRD asks *"is it a
capable, operable AI product a team would actually build on?"* The audit's
one-sentence verdict:

> **Apex has excellent primitives, but three structural problems recur at every
> layer: sophisticated machinery is built and tested yet never wired into the real
> run path; the AI core is missing table-stakes production capabilities; and
> single-node assumptions are baked into surfaces the product markets as
> multi-tenant.**

This document turns the audit's ~90 findings into numbered workstreams and testable
requirements, then hands them to three phased ticket docs
([RM-AIM-P1](../18-roadmap/v1.1/phase1-production-truth-tickets.md),
[RM-AIM-P2](../18-roadmap/v1.1/phase2-credible-ai-product-tickets.md),
[RM-AIM-P3](../18-roadmap/v1.1/phase3-ecosystem-scale-tickets.md)).

**This is a planning input, not a promise.** Requirements graduate to committed work
through the roadmap ([v1.1](../18-roadmap/v1.1/index.md)) and, where they change a
boundary contract, through an [ADR](../17-adr/index.md).

---

# 2. Problem Statement

The audit confirmed PRD-003's hardening shipped and holds. The remaining gaps are
not safety gaps — they are **capability, correctness, and operability** gaps:

1. **The AI core is a thin loop.** `run_agent` has no context-window management (the
   full history is cloned into every request and grows unbounded), no tokenizer,
   sequential-only tool execution, and no native Anthropic provider. Worst of all,
   **cost tracking is hardcoded to `$0` for every real provider**, which silently
   disables the per-project quota enforcement PRD-003 built.

2. **The strong machinery is dead code.** Container/gVisor/Firecracker sandboxes, the
   warm `SandboxPool`, and the `FairScheduler` are all built and tested — but the
   run path hardcodes `native_only()`, so none of them ever run. On Windows the
   "sandbox" enforces only a timeout. The distributed workflow/queue/lease layer has
   the same shape (PRD-003 Path A deferred it, correctly).

3. **The RAG stack is missing its middle.** No chunking (whole documents get one
   diluted embedding), no re-ranking, a semantic cache whose key omits the system
   prompt and embedding-model id (→ wrong-context hits), and no retrieval-quality
   evaluation.

4. **The evaluation harness is a prototype, not a gate.** Substring/exact scoring
   only; no LLM-as-judge, no baselines, no thresholds, no variance measurement.

5. **The ecosystem has no connective tissue.** No MCP (Model Context Protocol)
   client to reach external tool servers, no plugin authoring SDK/scaffold, a tiny
   read-biased builtin toolset (no `fs_write`, code-exec, or web-search).

6. **Operability and DX lag the code.** Version pinned at `0.1.0` despite a `v0.3.0`
   tag, no CHANGELOG, no release automation, no published container image; the
   dashboard and Windows are both absent from CI; the CLI reference doc misdescribes
   the primary entry point; the dashboard has zero tests and stores bearer tokens in
   `localStorage`.

The encouraging counterweight, again: because the abstractions are correct, **most
fixes are "wire the good primitive onto the path" or "add the missing stage," not
"redesign."**

---

# 3. Baseline (as of 2026-07-09)

- **Shipped & hardened (v0.1–v1.0 GA):** agent runtime, workflow engine, memory
  engine, LLM gateway, tool runtime, plugin engine + marketplace, multi-tenancy,
  events/webhooks, audit, secrets, KMS, the RM-GA-P1..P4 security/durability/
  contract floor, and (2026-07-09) the GA-003 residual findings + an S3-compatible
  backup destination.
- **The gap this PRD closes:** the difference between a hardened appliance and a
  capable, operable, extensible AI product — the AI-core depth, RAG maturity, eval
  rigor, ecosystem connectivity, sandbox activation, distributed correctness, and
  DX/release/UI hygiene that the audit found missing.

---

# 4. Goals & Non-Goals

## 4.1 Goals

- **Make the platform's own production claims true**: real cost accounting (so
  quotas mean something), context/token management (so long runs don't silently
  truncate), activated sandboxing (so isolation is real), and durable/graceful
  server behavior (so a restart isn't data loss).
- **Make it a credible AI product**: native Anthropic support, structured output,
  guardrails, a real RAG middle (chunking + reranking), and an evaluation gate that
  can actually catch a regression.
- **Make it extensible**: MCP, a plugin authoring SDK, and a richer, ergonomic tool
  surface.
- **Make it operable and adoptable**: consistent versioning + release automation +
  a published image, the dashboard and Windows in CI, accurate docs, and a UI with
  tests and a shared component system.

## 4.2 Non-Goals

- Billions-of-memories / thousands-of-concurrent-runs capacity engineering at real
  scale (PRD-002 post-GA Scale work).
- Marketplace monetization / billing (PRD-002).
- Multi-region residency (PRD-002).
- A cloud-KMS/HSM-backed root key (fast-follow; tracked in GA-003).

---

# 5. Requirement Conventions

- Requirements are `R-<workstream>.<n>`, testable, and cite the audit finding(s)
  they close (§12 traceability). Each maps to one or more tickets in the phase docs.
- Priority: **P0** = correctness/data-loss/quota-integrity blocker, **P1** =
  needed for a credible product, **P2** = quality bar, **P3** = fast-follow.
- Severity below mirrors the audit's own (High/Med/Low).
- "Done" = implemented, covered by an automated test that fails on regression, and
  the linked `docs/` spec updated (and for wire changes, both SDKs + `openapi.yaml`).

---

# 6. Workstreams

Twelve workstreams (A–L). Each requirement's ticket code and phase are in §12.

## WS-A — AI Core Runtime
- **R-A.1 (P0)** — Context-window management: add a tokenizer and a token-budgeted
  history compactor (drop/summarize oldest tool turns) before each model call; stop
  cloning unbounded history. *(Audit: no context mgmt, no token counting.)*
- **R-A.2 (P0)** — Execute independent tool calls in one turn concurrently, ordered
  by call id. *(Sequential tool loop.)*
- **R-A.3 (P1)** — Apply the agent manifest's `max_steps` as the default budget in
  `run_agent_inner` (not only via `apex-runtime`). *(max_steps ignored.)*
- **R-A.4 (P1)** — Step-level error recovery: retry a recoverable model-step error;
  on budget exhaustion, force a final tool-less answer instead of hard-erroring.
- **R-A.5 (P2)** — Richer streaming: emit tool-call-argument and reasoning/thinking
  events, not just content deltas.

## WS-B — Provider & Cost
- **R-B.1 (P0)** — Per-model price table; compute `cost_usd` from returned token
  usage in every provider. *(Cost hardcoded $0 → quotas are a no-op.)*
- **R-B.2 (P1)** — First-class `AnthropicProvider` (Messages API: native tool-use,
  system handling, prompt caching, extended thinking). *(No native Claude.)*
- **R-B.3 (P1)** — Add `response_format`/`tool_choice`/`json_schema` to `ChatRequest`
  and translate per provider (JSON mode, forced tool). *(No structured output.)*
- **R-B.4 (P2)** — Normalize/validate tool JSON-schema; surface tool-arg parse
  failures back to the model instead of passing `null`. *(Verbatim pass-through.)*
- **R-B.5 (P2)** — Multimodal content parts (image/audio) on `Message.content`.
- **R-B.6 (P3)** — Retry jitter + honor `Retry-After`; distinguish 429 vs 5xx.

## WS-C — Memory & RAG
- **R-C.1 (P1)** — Document chunking with parent-document linkage before embedding.
- **R-C.2 (P1)** — A re-ranking stage (cross-encoder or LLM reranker) after RRF.
- **R-C.3 (P1)** — Fix the semantic-cache key (include system prompt + tools) and
  stamp/verify the embedding-model id on every entry. *(Wrong-context hits.)*
- **R-C.4 (P2)** — BM25/TF-IDF in-process keyword search for backend parity.
- **R-C.5 (P2)** — Real timestamps on records + range/time metadata filters.
- **R-C.6 (P3)** — Incremental re-embedding / embedding-model migration.

## WS-D — Evaluation
- **R-D.1 (P1)** — LLM-as-judge + semantic-similarity scoring alongside substring.
- **R-D.2 (P1)** — Turn `apex-eval` into a real gate: golden baselines, pass-rate
  thresholds, variance-over-N, persisted score artifacts compared in CI.
- **R-D.3 (P2)** — Evaluate the RAG path (`run_agent_with_memory`) and manifest
  `max_steps`; add retrieval metrics (recall@k / nDCG / MRR).

## WS-E — Sandbox & Tool Runtime
- **R-E.1 (P0)** — Wire `SandboxManager::detect()` + `SandboxPool` into the agent/
  server run path so container/gVisor/Firecracker actually run. *(Dead code.)*
- **R-E.2 (P0)** — Windows Job Object for memory/CPU/PID limits in the non-Unix
  native path. *(Windows = timeout only.)*
- **R-E.3 (P1)** — Confined `fs_write` builtin; **R-E.4 (P1)** — a sandboxed
  code-execution tool; **R-E.5 (P2)** — a `#[derive(Tool)]`/schemars schema+typed-
  param ergonomics upgrade.
- **R-E.6 (P2)** — Document the platform matrix and fail closed when egress lockdown
  is unavailable (non-Linux).

## WS-F — Ecosystem & Extensibility
- **R-F.1 (P1)** — An MCP client tool-source (stdio/HTTP) proxying external tools
  into `ToolRegistry`. *(No external tool servers at all.)*
- **R-F.2 (P1)** — A plugin authoring SDK crate + `apex plugin new` scaffold
  (manifest + wasm build + digest computation + trust snippet). *(Format docs only.)*
- **R-F.3 (P2)** — A container capability loader (reuse `ContainerSandbox`).
- **R-F.4 (P2)** — One-shot `apex plugin publish` (sign + fill digests + emit trust).
- **R-F.5 (P3)** — Marketplace OSV/CVE feed keyed on SBOM `name@version`.

## WS-G — Server & Multi-Tenancy
- **R-G.1 (P0)** — Graceful shutdown/drain (`with_graceful_shutdown` + SIGTERM).
- **R-G.2 (P1)** — Durable async-run store (or documented non-durability), so a
  restart doesn't orphan pollable runs.
- **R-G.3 (P1)** — Durable webhook outbox + delivery worker with persisted DLQ.
- **R-G.4 (P1)** — API-key lifecycle: created/expires/revoked metadata, a revoke
  endpoint, rotation, last-used. *(Mint-only today.)*
- **R-G.5 (P1)** — Distributed rate limiting (shared store) for multi-node.
- **R-G.6 (P1)** — Per-tenant token quotas; enforce or remove the two dead quota
  dimensions; per-tenant rate tier.
- **R-G.7 (P2)** — Tenant-configurable daily-cost reset boundary (timezone).
- **R-G.8 (P2)** — Cache `FileApiKeyStore` in memory; served OpenAPI; request-path
  unwrap audit; idempotency-store write-amplification fix; move the ~2,260-line
  inline `lib.rs` test suite out.

## WS-H — Workflow Engine
- **R-H.1 (P0)** — Postgres connection pool (`deadpool`/`bb8`) + reconnect/health.
- **R-H.2 (P0)** — Sub-workflow recursion depth guard / ancestor-cycle detection.
- **R-H.3 (P1)** — TLS to Postgres; **R-H.4 (P1)** — fenced event-sequence
  generation (DB identity/sequence + lease-token fencing), replacing `MAX(seq)+1`.
- **R-H.5 (P1)** — Loop / for-each (map-over-collection) activity.
- **R-H.6 (P1)** — Dynamic (data-driven) fan-out.
- **R-H.7 (P2)** — Checkpoint size cap + out-of-line large activity outputs; event-
  log compaction + paged load; indexed `list()` columns + SQL-side pagination;
  `fire_at`-indexed timers + adaptive dispatch sleep.
- **R-H.8 (P3)** — Activity progress events; event-enum schema versioning.

## WS-I — Guardrails & Prompt Management
- **R-I.1 (P1)** — Content-safety / moderation / PII-redaction hooks on model input
  and output in the agent loop.
- **R-I.2 (P2)** — A prompt template/versioning registry (variables, versions, A/B).
- **R-I.3 (P1)** — Default the secrets store to encrypted-at-rest (plaintext becomes
  the explicit opt-out); **R-I.4 (P2)** — audit-log time-range + cursor pagination +
  indexed sink; **R-I.5 (P3)** — request-scoped secret channel (vsock/stdin) instead
  of `APEX_SECRET_*` env injection.

## WS-J — DX, SDKs & Release
- **R-J.1 (P1)** — Reconcile versioning: bump workspace/badges/SDKs to the real tag,
  add a maintained root CHANGELOG.
- **R-J.2 (P1)** — Release automation: tag-triggered signed binaries + a **published
  container image** (GHCR/Docker Hub) + npm/PyPI SDK publish + generated changelog.
- **R-J.3 (P1)** — Add the **dashboard** (build/lint/test) and a **Windows** matrix
  leg to CI.
- **R-J.4 (P2)** — SDK parity: async Python client, mutation retry (with
  `Idempotency-Key`), a `wait_for_completion` poll helper, TS `paginateAll`,
  coverage/benchmark tracking in CI.
- **R-J.5 (P2)** — SDK versioning tied to API version + per-SDK CHANGELOG + server/
  SDK skew warning; reconcile the Python PyPI-publish claim.
- **R-J.6 (P2)** — Regenerate `docs/11-cli/commands.md` from the real clap tree;
  add per-doc shipped/aspirational status front-matter; add a top-of-README
  5-minute quickstart; unify the `repository` URL across manifests.
- **R-J.7 (P3)** — Decide/document Go/Java clients (roadmap or non-goal).

## WS-K — Dashboard / UI
- **R-K.1 (P1)** — Move bearer token off `localStorage` (in-memory/session or
  BFF-issued httpOnly cookie). *(XSS-exfiltratable.)*
- **R-K.2 (P1)** — UI test coverage: service specs (SSE parser, manifest round-trip)
  + a smoke e2e; drop the global `skipTests:true`.
- **R-K.3 (P2)** — Shared component library (StatusPill/Tabs/Modal/Table/empty/
  loading/error), replacing the duplicated `statusClass`/`errText` patterns and the
  native `confirm()`.
- **R-K.4 (P2)** — Share API types with `sdks/typescript` (or generate from OpenAPI);
  replace string-built YAML with a real (de)serializer; central HTTP error handling
  (no swallowed errors).
- **R-K.5 (P2)** — Audit-log viewer surface; **R-K.6 (P2)** — responsive/mobile
  breakpoints; **R-K.7 (P2)** — accessibility pass (label associations, aria on icon
  buttons, modal focus mgmt).
- **R-K.8 (P3)** — Prompt playground; live nav badges (or remove the fakes); i18n
  decision; icon sprite for bundle hygiene.

## WS-L — Observability & Operability
- **R-L.1 (P1)** — Per-tenant/per-project metric labels (bounded cardinality).
- **R-L.2 (P2)** — Queue-depth / in-flight / pending-timer / webhook-DLQ gauges.
- **R-L.3 (P2)** — Spans around Postgres/queue/dispatcher operations.
- **R-L.4 (P2)** — A systemd unit + install script for the single-node appliance;
  an operator upgrade/backup/migration runbook.
- **R-L.5 (P3)** — SLO / error-budget burn-rate metrics + starter alert rules;
  Helm HA/TLS templating; a minimal Terraform module (or explicit scope-out).

---

# 7. Distributed Scale-Out (folded from PRD-003 Path B)

PRD-003 deferred the distributed platform (Path B) to a "v1.1 Scale-Out" milestone.
Several of its wiring tickets overlap this PRD's correctness work and are absorbed
here as **P1** items where they are single-node-correctness bugs *today* (R-G.5
distributed rate limiting, R-H.1 pooling, R-H.4 fencing), and left as PRD-002
capacity work where they are pure scale. The multi-replica shared-catalog promotion
(PRD-003 R-5.1/R-5.2) remains gated on Product demand and is tracked, not committed,
in [v1.1 §Scale-Out](../18-roadmap/v1.1/index.md).

---

# 8. Prioritization & Sequencing

Three phases, ordered by dependency (calendar dates omitted per house convention).
Within a phase, items are parallelizable.

- **Phase 1 — Make production claims true (P0/P1).** The fixes that make existing
  features actually work: **R-B.1 (cost table) is the single highest-leverage item**
  — it silently disables quota enforcement everywhere. Plus context/token mgmt,
  sandbox activation, graceful shutdown, durable async runs, Postgres pool, release
  reconciliation, and dashboard+Windows CI. → [RM-AIM-P1](../18-roadmap/v1.1/phase1-production-truth-tickets.md)
- **Phase 2 — Credible AI product (P1/P2).** Anthropic provider, RAG chunking +
  reranking, semantic-cache correctness, structured output, guardrails, the eval
  gate, distributed rate limiting, per-tenant quotas. → [RM-AIM-P2](../18-roadmap/v1.1/phase2-credible-ai-product-tickets.md)
- **Phase 3 — Ecosystem & scale (P2/P3).** MCP, plugin SDK, workflow loops/fan-out,
  encrypted-secret default, UI component library + audit viewer + responsive,
  SDK parity, docs, systemd/runbooks, observability gauges. → [RM-AIM-P3](../18-roadmap/v1.1/phase3-ecosystem-scale-tickets.md)

**The trap to avoid:** shipping more surface (or more UI) before R-B.1 lands — every
cost/quota number the product reports until then is fiction.

---

# 9. Exit Criteria

This PRD's scope is met when:

1. Every real provider reports accurate `cost_usd`; per-project quotas enforce real
   spend (WS-B/R-B.1). A long tool loop never silently exceeds the context window
   (WS-A/R-A.1).
2. The agent run path uses the strongest available sandbox for the run's trust
   class; Windows runs enforce real resource limits (WS-E).
3. Claude is a first-class provider; structured output and moderation hooks exist
   (WS-B, WS-I).
4. Memory chunks + reranks; the semantic cache never serves a wrong-context hit
   (WS-C). `apex-eval` fails CI on a real quality regression (WS-D).
5. A server restart loses no pollable run or pending webhook; shutdown drains
   (WS-G). Multi-node rate limits and quotas are correct (WS-G/WS-H).
6. Versioning/CHANGELOG/release automation exist and a container image is published;
   the dashboard and Windows are in CI (WS-J).
7. MCP and a plugin SDK exist; the UI has tests and a shared component system (WS-F,
   WS-K).

---

# 10. Risks & Assumptions

- **R:** R-B.1 (cost table) changes quota behavior from "always $0, never blocks" to
  "actually blocks" — a behavior change operators may not expect. **Mitigation:**
  ship behind a documented rollout; log computed cost before enforcing.
- **R:** WS-A context compaction can change agent outputs (summarizing history).
  **Mitigation:** make the strategy configurable; default to lossless
  drop-oldest-tool-turns before summarization.
- **R:** WS-E activating strong sandboxes changes latency/perf characteristics.
  **Mitigation:** the warm `SandboxPool` already exists to amortize; benchmark first.
- **A:** The trait-port architecture holds (PRD-003 confirmed), so provider/sandbox/
  reranker additions are new impls behind existing traits, not redesigns.
- **A:** The audit's file:line evidence (captured 2026-07-09) is current; each ticket
  re-verifies before implementing.

---

# 11. Related

- [PRD-001](prd.md) · [PRD-002](prd-future.md) · [PRD-003](prd-ga-hardening.md)
- [`18-roadmap/v1.1/index.md`](../18-roadmap/v1.1/index.md) — the milestone + phase ticket docs
- [`18-roadmap/v1.0.md`](../18-roadmap/v1.0.md) — the GA milestone this builds on

---

# 12. Traceability Matrix — Findings → Requirements → Tickets

Severity is the audit's. Phase/ticket codes are authoritative; the phase docs
implement exactly these.

| Audit finding (abbrev.) | Sev | Req | Ticket | Phase |
|-------------------------|-----|-----|--------|-------|
| No context-window mgmt; no tokenizer | High | R-A.1 | AIC-101 | 1 |
| Sequential tool-call execution | High | R-A.2 | AIC-102 | 1 |
| Manifest `max_steps` ignored by `run_agent` | Med | R-A.3 | AIC-103 | 1 |
| No step-error recovery / budget-exhaust discards work | Med | R-A.4 | AIC-201 | 2 |
| Streaming is content-only | Low | R-A.5 | AIC-202 | 2 |
| Cost tracking hardcoded $0 (no price table) | High | R-B.1 | PRV-101 | 1 |
| No native Anthropic provider | High | R-B.2 | PRV-201 | 2 |
| No structured output / tool_choice | Med | R-B.3 | PRV-202 | 2 |
| Tool-schema pass-through; null-swallow | Med | R-B.4 | PRV-203 | 2 |
| No multimodal | Med | R-B.5 | PRV-204 | 2 |
| Retry no jitter / ignores Retry-After | Low | R-B.6 | PRV-205 | 2 |
| Memory: no chunking | High | R-C.1 | RAG-201 | 2 |
| Memory: no re-ranking | High | R-C.2 | RAG-202 | 2 |
| Semantic-cache key / embedding-model mismatch | High | R-C.3 | RAG-203 | 2 |
| Naive keyword search (no BM25) | Med | R-C.4 | RAG-204 | 2 |
| No real timestamps / range filters | Low | R-C.5 | RAG-205 | 2 |
| No incremental re-embedding | Low | R-C.6 | RAG-301 | 3 |
| No retrieval-quality eval | Med | R-D.3 | EVL-203 | 2 |
| Eval scoring substring-only (no LLM-judge) | High | R-D.1 | EVL-201 | 2 |
| Eval not a regression gate | High | R-D.2 | EVL-202 | 2 |
| Strong sandboxes dead code (native_only) | High | R-E.1 | SBX-101 | 1 |
| Windows: no resource limits | High | R-E.2 | SBX-102 | 1 |
| No `fs_write` builtin | Med | R-E.3 | SBX-301 | 3 |
| No code-execution tool | Med | R-E.4 | SBX-302 | 3 |
| Custom-tool ergonomics (no derive) | Med | R-E.5 | SBX-303 | 3 |
| Egress lockdown Linux-only, silent | Med | R-E.6 | SBX-304 | 3 |
| No MCP / external tool servers | High | R-F.1 | ECO-301 | 3 |
| No plugin authoring SDK/scaffold | High | R-F.2 | ECO-302 | 3 |
| No container/microVM plugin loaders | Med | R-F.3 | ECO-303 | 3 |
| Plugin signing UX friction | Med | R-F.4 | ECO-304 | 3 |
| Marketplace scanner static-only (no CVE) | Med | R-F.5 | ECO-305 | 3 |
| No graceful shutdown/drain | High | R-G.1 | SRV-101 | 1 |
| Async runs not durable | High | R-G.2 | SRV-102 | 1 |
| Webhook delivery in-process, no outbox | High | R-G.3 | SRV-103 | 1 |
| API keys no expiry/rotation/revocation | High | R-G.4 | SRV-104 | 1 |
| Rate limiter in-process only | High | R-G.5 | SRV-201 | 2 |
| Only 2/4 quota dims; no token quota | Med | R-G.6 | SRV-202 | 2 |
| Daily-cost window UTC-only | Med | R-G.7 | SRV-203 | 2 |
| FileApiKeyStore reads file per request | Med | R-G.8 | SRV-302 | 3 |
| No served OpenAPI; no WebSocket | Med | R-G.8 | SRV-303 | 3 |
| lib.rs ~86% inline test | Med | R-G.8 | SRV-304 | 3 |
| Idempotency full-file write per req | Med | R-G.8 | SRV-305 | 3 |
| Request-path unwraps | Low | R-G.8 | SRV-306 | 3 |
| In-process concurrency slots | Low | R-G.6 | SRV-307 | 3 |
| Postgres single client, no pool | High | R-H.1 | WFL-101 | 1 |
| Unbounded sub-workflow recursion | High | R-H.2 | WFL-102 | 1 |
| No TLS to Postgres | High | R-H.3 | WFL-103 | 1 |
| Event seq via MAX+1 race | Med | R-H.4 | WFL-104 | 1 |
| No loop/for-each | High | R-H.5 | WFL-301 | 3 |
| No dynamic fan-out | High | R-H.6 | WFL-302 | 3 |
| Checkpoint write-amp / no payload cap | Med | R-H.7 | WFL-303 | 3 |
| Event log no compaction / full load | Med | R-H.7 | WFL-304 | 3 |
| list() scans all checkpoints | Med | R-H.7 | WFL-305 | 3 |
| Timer/schedule poll O(N) / accuracy | Med | R-H.7 | WFL-306 | 3 |
| No activity progress events | Low | R-H.8 | WFL-307 | 3 |
| Event enum no versioning | Low | R-H.8 | WFL-308 | 3 |
| apex-runtime `ai` activity underspecified | Med | R-A.4 | RUN-201 | 2 |
| Sub-agent observability lost (NullSink, $0) | Low | R-B.1 | RUN-202 | 2 |
| No guardrails/moderation/PII | Med | R-I.1 | SAF-201 | 2 |
| No prompt template/versioning | Med | R-I.2 | SAF-202 | 2 |
| Default secrets plaintext | High | R-I.3 | SEC-101 | 1 |
| Audit query no time-range/pagination/scale | Med | R-I.4 | SEC-301 | 3 |
| Secret injection env-var leak surface | Low | R-I.5 | SEC-302 | 3 |
| Version 0.1.0 vs v0.3.0; no CHANGELOG | High | R-J.1 | DX-101 | 1 |
| No release automation / published image | High | R-J.2 | DX-102 | 1 |
| Dashboard + Windows absent from CI | High | R-J.3 | DX-103 | 1 |
| SDK parity (async py, retry, poll helper) | Med | R-J.4 | DX-301 | 3 |
| No coverage/benchmark tracking | Med | R-J.4 | DX-302 | 3 |
| SDK versioning strategy absent | Med | R-J.5 | DX-303 | 3 |
| commands.md out of sync | Med | R-J.6 | DX-304 | 3 |
| Docs status front-matter; quickstart; repo URL | Low | R-J.6 | DX-305 | 3 |
| Go/Java clients decision | Low | R-J.7 | DX-306 | 3 |
| Token in localStorage | High | R-K.1 | UI-101 | 1 |
| Zero UI test coverage | High | R-K.2 | UI-102 | 1 |
| No shared component abstraction | Med | R-K.3 | UI-301 | 3 |
| API types drift; string-built YAML; swallowed errors | Med | R-K.4 | UI-302 | 3 |
| No audit-log viewer | High | R-K.5 | UI-303 | 3 |
| Weak responsive/mobile | Med | R-K.6 | UI-304 | 3 |
| Accessibility thin | Med | R-K.7 | UI-305 | 3 |
| Playground / nav badges / i18n / icon sprite | Low | R-K.8 | UI-306 | 3 |
| No per-tenant metrics | Med | R-L.1 | OBS-201 | 2 |
| No queue-depth/DLQ gauges | Med | R-L.2 | OBS-301 | 3 |
| Uneven trace coverage | Low | R-L.3 | OBS-302 | 3 |
| No systemd/install; no upgrade runbook | High | R-L.4 | DEP-301 | 3 |
| No SLO burn; Helm HA/TLS; Terraform | Low | R-L.5 | DEP-302 | 3 |

---

# 13. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-09 | Initial AI-platform-maturity PRD: a 2026-07-09 five-front engineering audit's ~90 findings mapped to 12 workstreams / testable requirements, phased into three ticket docs (RM-AIM-P1/P2/P3), with the distributed Scale-Out fold-in and a full findings→requirements→ticket traceability matrix |
