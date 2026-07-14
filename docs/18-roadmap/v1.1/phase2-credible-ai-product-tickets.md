<!--
File: docs/18-roadmap/v1.1/phase2-credible-ai-product-tickets.md
Document ID: RM-AIM-P2
-->

# Phase 2 — Credible AI Product: Implementation Tickets

**Document ID:** RM-AIM-P2
**File Path:** `docs/18-roadmap/v1.1/phase2-credible-ai-product-tickets.md`
**Version:** 1.21.0
**Status:** In progress — WS-B, WS-C, WS-D, WS-A/runtime, and WS-G fully done; SAF-201 done; remaining: SAF-202, OBS-201
**Owner:** Engineering (AI / Platform)
**Last Updated:** 2026-07-13

---

# Purpose

Phase 2 of [PRD-004 §8](../../01-product/prd-ai-platform-maturity.md) — the
capabilities that make Apex a *credible* AI product rather than a thin loop: native
Anthropic support, structured output, a real RAG middle (chunking + reranking), a
correct semantic cache, guardrails, an evaluation *gate*, and correct multi-node
quotas.

Covers **WS-A** (AI-core recovery/streaming), **WS-B** (providers), **WS-C**
(RAG), **WS-D** (eval), **WS-G/R-G.5..7** (quotas), **WS-I** (guardrails/prompts),
**WS-L/R-L.1** (per-tenant metrics), and the shared-executor `ai`-activity fixes.

Format matches [RM-GA-P2](../v1.0/phase2-durability-execution-tickets.md).
Depends on Phase 1 — especially PRV-101 (real cost) and AIC-101 (token counting).

---

# Sequencing at a glance

```
PRV-201 (Anthropic) ─┬─ provider work (parallel)
PRV-202 (structured out) │
PRV-203 (schema norm)     │
PRV-204 (multimodal)      │
PRV-205 (retry jitter)   ┘
RAG-201 (chunking) ── RAG-202 (rerank) ── RAG-205 (metadata/time)
RAG-203 (semantic-cache fix) ─ independent
RAG-204 (BM25) ─ independent
EVL-201 (LLM-judge) ── EVL-202 (regression gate) ── EVL-203 (RAG/max_steps eval)
SAF-201 (guardrails) ─ SAF-202 (prompt registry)
SRV-201 (distributed rate limit) ─ SRV-202 (token quotas) ─ SRV-203 (tz window)
AIC-201 (step recovery) ─ AIC-202 (rich streaming) ─ RUN-201/202 (ai activity)
OBS-201 (per-tenant metrics)
```

---

# WS-B — Providers

## PRV-201 `[P1]` — First-class `AnthropicProvider` — **DONE (2026-07-13)**

**Problem.** Only mock, OpenAI-compatible, and local mistralrs providers exist
(`crates/apex-provider/src/lib.rs:12-33`); Claude is reachable only via an
OpenAI-compatible shim, losing native tool-use, system-prompt handling, prompt
caching, and extended thinking. (PRD-004 R-B.2; audit High.)

**Change.** Add an `AnthropicProvider` implementing `AIProvider` against the Messages
API: native `tools`/`tool_choice`, `system`, prompt caching, and (optional) extended
thinking; translate `ChatRequest`/`ToolSpec` to/from Anthropic shapes; emit real
streaming deltas via `chat_stream`.

**Acceptance criteria.** A gated integration test (or a recorded-fixture test) drives
a tool round-trip through the Anthropic provider; `cost_usd` uses PRV-101's price
table for Claude models.

**Files.** `crates/apex-provider/src/` (new `anthropic.rs`), `gateway.rs` resolution.
**Size.** L. **Depends on:** PRV-101.

**Implementation notes (2026-07-13).** New `apex-provider::anthropic` module: an
`AnthropicProvider` implementing `AIProvider` against the native Messages API
(`POST /v1/messages`, `x-api-key` + `anthropic-version` headers), constructed from
`ANTHROPIC_API_KEY`/`APEX_ANTHROPIC_BASE_URL`. Translation: `Role::System` messages
hoist to the top-level `system` block list; assistant `tool_calls` become
`tool_use` blocks (JSON-string `arguments` ↔ JSON-object `input`); `Role::Tool`
results become `tool_result` blocks in a `user` turn, with consecutive results
merged into one turn (Anthropic requires all parallel-call results in a single
user message); `max_tokens` (required by the API) defaults to 4096 when the
normalized request leaves it unset; `stop_reason` normalizes `end_turn`→`stop`,
`tool_use`→`tool_calls`, everything else passes through. **Prompt caching** is on
by default (`with_prompt_caching(false)` to disable): `cache_control: {type:
"ephemeral"}` breakpoints on the last tool + last system block — the stable prefix
of an agent loop — and `cost_usd` weights cache writes/reads at their real
1.25×/0.1× input rates while `prompt_tokens` reports the true three-category sum.
`chat_stream` parses the real SSE event stream (`message_start` /
`content_block_start` / `text_delta` / `input_json_delta` / `message_delta` /
`message_stop`, `error` events surfaced as stream errors). 429/5xx (incl. 529
`overloaded_error`) classify transient, other 4xx permanent — same resilience
contract as `OpenAiProvider`. Wiring: `Gateway::from_env()` tries OpenAI (existing
precedence, unchanged), then Anthropic, then mock; `resolve_model` maps
fast/balanced/frontier → `claude-haiku-4-5`/`claude-sonnet-5`/`claude-opus-4-8`;
`PriceBook::with_defaults()` gained current Claude prices (`claude-fable-5`,
`claude-opus-4` prefix, `claude-sonnet-5`/`-4` prefix, `claude-haiku-4-5`); the CLI
gained an explicit `agents run --local --provider anthropic`. Anthropic has no
embeddings/images API, so those trait defaults ("unsupported") stand — semantic
caching degrades to a live call by design. **Acceptance:**
`tests/anthropic_messages.rs::tool_round_trip_via_recorded_fixtures` drives a full
model → tool → model round-trip through the provider against recorded fixtures,
asserting both the parsed tool call and the wire bodies it sends back
(`tool_use_id` correlation, system hoisting, cache breakpoints), with `cost_usd`
computed from PRV-101's table for `claude-opus-4-8` including the 0.1× cache-read
rate; `streams_text_deltas_then_done`/`streams_tool_use_assembled_from_partial_json`
cover the SSE path; 15 unit tests cover the translation edge cases. Verified live
end to end: `apex agents run --local --provider anthropic` against a canned local
Messages-API server streamed real deltas and reported the exact table-computed
cost. Deferred to their own tickets: `tool_choice`/structured output (PRV-202) and
extended thinking / multimodal content parts (PRV-204).

## PRV-202 `[P1]` — Structured output / forced tool — **DONE (2026-07-13)**

**Problem.** `ChatRequest` has no `response_format`/`tool_choice`/`json_schema`
(`crates/apex-provider/src/types.rs:107-123`); mistralrs hardcodes `ToolChoice::Auto`
(`mistralrs_provider.rs:166`). JSON mode and "must call tool X" can't be requested.
(PRD-004 R-B.3; audit Med.)

**Change.** Add `response_format` and `tool_choice` to `ChatRequest`; translate per
provider (OpenAI JSON/structured-output, Anthropic tool_choice, mistralrs where
supported).

**Acceptance criteria.** A test asserts a JSON-schema-constrained request returns
schema-valid output (against a provider that supports it) and that forced-tool
selects the named tool.

**Files.** `crates/apex-provider/src/{types.rs,openai.rs,anthropic.rs,mistralrs_provider.rs}`.
**Size.** M. **Depends on:** none.

**Implementation notes (2026-07-13).** `ChatRequest` gained two optional fields
with builder helpers (`with_tool_choice`/`with_response_format`): `ToolChoice`
(`Auto`/`None`/`Required`/`Tool(name)`) and `ResponseFormat` (`JsonObject` |
`JsonSchema { name, schema }`). Per-provider translation, all fail-closed
(`Error::Invalid` — permanent, so the gateway never fails over to a provider
that would silently change the semantics) where a backend lacks an equivalent:
**OpenAI** — `tool_choice` (`"auto"`/`"none"`/`"required"`/
`{type:"function",function:{name}}`) and `response_format`
(`{type:"json_object"}` / `{type:"json_schema", json_schema:{name, schema,
strict:true}}`); **Anthropic** — `tool_choice` (`auto`/`none`/`any`/`tool`) and
`output_config.format` (`json_schema`, schema bare — `name` is OpenAI-only);
schema-less `JsonObject` is rejected (the Messages API has no JSON mode), and
`request_body` became fallible for it; **mistral.rs** — `set_tool_choice`
(`Auto`/`None`/`Tool(spec)`, resolving the named tool against the advertised
list and rejecting unknown names; `Required` has no mistral.rs equivalent →
`Invalid`) and `set_constraint(Constraint::JsonSchema)` — real constrained
decoding, compile-checked with `--features mistralrs` (not blind-edited).
**Cache correctness:** both fields joined the gateway's exact `cache_key` *and*
the semantic `param_key` — a response produced under one constraint set must
never be served for another; proven by
`gateway::tests::exact_cache_does_not_cross_output_constraints`. **Acceptance:**
`tests/anthropic_messages.rs::forced_tool_choice_selects_the_named_tool`
(recorded fixture: the wire carries `{"type":"tool","name":"calc"}` and the
response selects exactly the named tool) and
`json_schema_constrained_request_returns_schema_valid_output` (the wire carries
the schema in `output_config.format`; the answer is validated against it —
required fields, types, `additionalProperties: false`); OpenAI wire shapes are
unit-tested (`encodes_tool_choice_variants`/`encodes_response_format_variants`),
as are Anthropic's (`encodes_tool_choice_variants`/
`json_schema_response_format_becomes_output_config`/
`schemaless_json_mode_fails_closed_as_invalid`). Not yet surfaced in the agent
manifest/YAML DSL — callers set the fields programmatically; manifest wiring
can ride a later slice (e.g. SAF-202's prompt registry) once a consumer needs it.

## PRV-203 `[P2]` — Tool-schema normalization + surfaced arg-parse errors — **DONE (2026-07-13)**

**Problem.** Tool `parameters` JSON is forwarded verbatim to providers
(`openai.rs:62-79`); no normalization/`strict` mode. Malformed tool arguments are
swallowed to `Value::Null` (`crates/apex-agent/src/runtime.rs:335`), so a tool
silently receives null. (PRD-004 R-B.4; audit Med.)

**Change.** Validate/normalize tool JSON-schema (strip unsupported keywords, optional
`strict`); on an arg-parse failure, feed the error back to the model instead of
passing `null`.

**Acceptance criteria.** A test asserts a malformed tool-arg produces a
model-visible error turn (not a null-arg tool invocation).

**Files.** `crates/apex-provider/src/openai.rs`, `crates/apex-agent/src/runtime.rs`.
**Size.** M. **Depends on:** none.

**Implementation notes (2026-07-13).** Two halves. **(1) Schema normalization:**
new `apex-provider::schema` module with `normalize_strict(schema)` — a pure,
recursive rewrite into the vendor strict-mode JSON-Schema subset: strips the
keywords strict validators reject (`minimum`/`maximum`/`multipleOf`,
`minLength`/`maxLength`/`pattern`/`format`, array/property bounds,
`patternProperties`, `default`), recursing through `properties`/`items`/
`anyOf`/`allOf`/`oneOf`/`not`/`if`/`then`/`else`/`$defs`/`definitions`, and
closes every object node (`additionalProperties: false` + `required` listing
every declared property, the OpenAI strict rule; Anthropic's is a compatible
subset). Careful detail: property *names* that collide with keyword names
(a property literally called `format`) survive — only keyword-position uses
are stripped. Opt-in via a new `ToolSpec.strict: bool` (`#[serde(default)]`,
back-compat): when set, `OpenAiProvider` emits `function.strict: true` +
the normalized `parameters`, and `AnthropicProvider` emits top-level
`strict: true` + the normalized `input_schema` (the prompt-caching breakpoint
still lands on the last tool). Deliberately **not** applied to non-strict
tools: providers ignore unknown keywords in normal mode but honor bounds like
`minimum`, so unconditional stripping would silently discard real constraints.
mistral.rs needs no normalization (its `Function.parameters` is passthrough
and it has no strict mode). **(2) Surfaced arg-parse errors:**
`execute_tool_call` (`crates/apex-agent/src/runtime.rs`) no longer swallows
malformed tool arguments to `Value::Null` — a parse failure now returns a
failed `ToolOutcome` whose text carries the serde error and an instruction to
re-issue the call, which the existing loop feeds back as the tool-result turn,
so the model sees and can correct it; the tool itself is never invoked. An
*empty* argument string is the conventional no-arg call and still invokes the
tool with `{}` (providers' stream accumulators already normalize empty to
`"{}"`, but a scripted/foreign provider may not). **Acceptance:**
`crates/apex-agent/tests/tool_loop.rs::malformed_tool_arguments_surface_as_a_model_visible_error_turn`
drives the real `run_agent` loop with a recording tool and a scripted provider
issuing `{"ping": pong` — asserts the tool never executed, the model's next
turn observed the "not valid JSON" error text, and the sink saw a failed tool
result; `empty_tool_arguments_invoke_with_an_empty_object` guards the no-arg
convention. Schema normalization is covered by 5 `schema::tests` unit tests
plus per-adapter wire-shape tests
(`strict_tool_emits_strict_flag_and_normalized_schema` /
`strict_tool_emits_strict_flag_and_normalized_input_schema`), and the
`mistralrs` feature still compile-checks. Nothing sets `strict: true` on the
run path yet — `resolve_tools` advertises registry tools non-strict;
per-tool/manifest opt-in can ride a later slice once a consumer wants
guaranteed argument shapes.

## PRV-204 `[P2]` — Multimodal content parts — **DONE (2026-07-13)**

**Problem.** `Message.content` is `Option<String>` (`types.rs:31`); no image/audio
parts. (PRD-004 R-B.5; audit Med.)

**Change.** Model content as a list of typed parts (text/image/audio) with backward-
compatible string coercion; translate per provider.

**Acceptance criteria.** A test round-trips an image content part through a
multimodal-capable provider path.

**Files.** `crates/apex-provider/src/types.rs` + provider translators. **Size.** M.
**Depends on:** none.

**Implementation notes (2026-07-13).** `Message` gained a `parts: Vec<ContentPart>`
field (`#[serde(default, skip_serializing_if = "Vec::is_empty")]`, so a text-only
`Message` keeps its old wire shape and old callers are untouched) alongside the
existing `content: Option<String>`, plus a `with_part` builder. `ContentPart` is an
internally-tagged enum (`Text`/`ImageUrl`/`Image { media_type, data }`/
`Audio { media_type, data }`) with `text()`/`image_url()`/`image_base64()`/
`audio_base64()` constructors. Only `Role::User` turns may carry parts — every
provider rejects parts on any other role fail-closed (`Error::Invalid`, permanent,
same contract as PRV-202's constraints) rather than silently dropping them.
Per-provider translation, rendered as `content` text (if any) first, then each part
in order: **Anthropic** — `image_url`/`image_base64` become `image` blocks
(`source: {type: "url"|"base64", ...}`); `Audio` has no Messages-API equivalent and
fails closed. **OpenAI** — `image_url` passes through; `image_base64` rides as a
`data:` URI inside `image_url` (OpenAI has no separate inline-image block); `Audio`
becomes `input_audio` with the bare format name (`audio/wav` → `wav`, since OpenAI
wants a format string, not a MIME type). **mistral.rs** — this backend loads a
text-only GGUF pipeline (no `VisionModelBuilder` wiring yet), so any message with
parts fails closed rather than silently ignoring the image/audio; a real
vision-capable local model is a later slice. **Acceptance:**
`tests/anthropic_messages.rs::image_content_part_round_trips_through_the_messages_api`
drives a real image through `AnthropicProvider::chat` against a recorded fixture,
asserting the wire carries the base64 image block after the text block and the
model's answer parses back through the normal response path with real cost;
`anthropic::tests::multimodal_user_message_encodes_as_image_blocks`/
`audio_parts_fail_closed_as_invalid`/`parts_on_a_non_user_turn_fail_closed` and the
equivalent `openai::tests::multimodal_user_message_encodes_as_content_blocks`/
`parts_on_a_non_user_turn_fail_closed` cover the translation + fail-closed edge
cases; `types::tests::message_with_parts_round_trips`/
`message_without_parts_keeps_its_old_wire_shape_and_deserializes_back`/
`content_parts_serialize_internally_tagged` cover the wire-shape/back-compat
contract. `ChatRequest`/message-construction call sites across `apex-agent`
(`context.rs`, `runtime.rs`, `tool_loop.rs` tests, `tokenizer.rs` tests) updated for
the new field; full workspace build + `apex-provider`/`apex-agent` suites (108
tests) pass. Not yet surfaced in the agent manifest YAML DSL or the CLI — callers
attach parts programmatically; manifest wiring (e.g. `--image <path>`) can ride a
later slice once a consumer needs it.

## PRV-205 `[P3]` — Retry jitter + `Retry-After` — **DONE (2026-07-13)**

**Problem.** Backoff is pure exponential, no jitter
(`crates/apex-provider/src/resilience.rs:46-49`); `is_transient` keys only on the
`Error::Provider` variant (`gateway.rs:607-609`), not distinguishing 429 vs 5xx nor
honoring server backoff. (PRD-004 R-B.6; audit Low.)

**Change.** Add jitter; parse and honor `Retry-After`; classify 429 vs 5xx.

**Acceptance criteria.** A test asserts jittered delays and that a `Retry-After` hint
is respected.

**Files.** `crates/apex-provider/src/{resilience.rs,gateway.rs}`. **Size.** S.
**Depends on:** none.

**Implementation notes (2026-07-13).** `Error::Provider(String)` became a struct
variant `Provider { message: String, retry_after_ms: Option<u64> }` — `Error::provider(msg)`
keeps its old signature (`retry_after_ms: None`, so all ~14 existing call sites across
the workspace are untouched) and a new `Error::provider_with_retry_after(msg, ms)`
carries a server-specified delay. **Jitter:** a new `Jitter` trait (`jitter_ms(bound_ms)
-> u64`) is injected into `Gateway` (`with_jitter`, default `RandomJitter` — the one
place in this crate that reads the process RNG, kept behind the trait so "no ambient
randomness in core logic" still holds for everything else); `RetryConfig::backoff`
stays pure/deterministic (unchanged, existing test intact), and a new
`backoff_with_jitter(attempt, jitter)` implements AWS-style "full jitter" — a uniform
draw in `[0, backoff(attempt)]` — which `Gateway::try_provider`'s retry loop now
actually sleeps on instead of the raw exponential value. `FixedJitter(ms)` is the
deterministic test double. **Retry-After:** `resilience::parse_retry_after_ms` parses
the delay-seconds form of the header (the form every real LLM rate-limit response
uses; the HTTP-date form is explicitly out of scope, treated as absent) into
milliseconds; both `OpenAiProvider` and `AnthropicProvider` capture it from the
response headers *before* consuming the body and thread it through
`classify_http_error` (now 3-arg), which attaches it to the `Error::Provider` it
returns for a 429/5xx. `Gateway::try_provider` checks for this hint first and, when
present, sleeps for exactly that duration in place of its own jittered backoff
(resilience §4: "a server-specified Retry-After overrides backoff entirely");
`embed`/`generate_image` (which have no retry/failover pipeline in the first place)
pass `None` since there's nothing downstream to honor it. **Acceptance:**
`gateway::tests::retry_backoff_uses_the_injected_jitter_source` and
`retry_after_hint_overrides_backoff` (both `#[tokio::test(start_paused = true)]`,
asserting exact elapsed virtual time against a retry config whose raw exponential
cap is deliberately huge — 10s — so the test would time out/mismatch if either
mechanism weren't actually wired in) plus `resilience::tests::
jittered_backoff_is_deterministic_under_a_fixed_source`/`random_jitter_stays_within_bounds`/
`retry_after_header_parses_as_milliseconds`/`missing_or_non_numeric_retry_after_is_none`.
Changing the `Provider` variant's shape touched 7 files total (`apex-common/error.rs`,
`apex-provider/{gateway,openai,anthropic}.rs`, `apex-provider/tests/chaos.rs`,
`apex-server/agents.rs`'s error-envelope mapping, `apex-plugin/rekor.rs`'s direct
tuple constructions switched to the `Error::provider(...)` helper) — full workspace
build/test/clippy/fmt all pass. 429-vs-5xx classification itself was already correct
pre-existing behavior (`classify_http_error` already routed both to `Error::Provider`
and other 4xx to `Error::Invalid`); this ticket's classification gap was really about
capturing the Retry-After hint at the point that classification happens, which is
now done.

---

# WS-C — Memory & RAG

## RAG-201 `[P1]` — Document chunking with parent-document linkage — **DONE (2026-07-13)**

**Problem.** `remember_full` embeds the entire `content` as one vector
(`crates/apex-memory/src/engine.rs:91-105`); long docs get one diluted embedding, no
splitter, no parent linkage. (PRD-004 R-C.1; audit High.)

**Change.** Add a configurable splitter (token/char windows + overlap) that stores
chunk records linked to a parent document; retrieval returns chunks and can expand to
the parent.

**Acceptance criteria.** A test asserts a long document is split into linked chunks and
that retrieval scores a relevant chunk above an irrelevant one from the same document.

**Files.** `crates/apex-memory/src/engine.rs` + record model. **Size.** L.
**Depends on:** none.

**Implementation notes (2026-07-13).** New `apex-memory::chunk` module:
`ChunkPolicy { max_chars, overlap_chars }` (default 1200/200 — ~300/~50 tokens at
the ~4 chars/token heuristic) and `split()` — a pure, deterministic splitter over
**character windows with word-boundary snapping** (characters as a dependency-free
token proxy, the same documented-estimate stance as `apex_provider`'s
`HeuristicTokenizer`; a word is never cut mid-way, an over-long single word is kept
whole, and overlap is clamped below the window so every step provably advances).
**Record model:** `MemoryRecord` gained `parent_id: Option<String>` (chunk → parent
link, `skip_serializing_if` for wire back-compat) and `is_parent: bool` (marks the
full-document record). **Ingestion:** `MemoryEngine::remember_document` (returning
`DocumentIngest { parent_id, chunk_ids }`) stores the verbatim document as a parent
record with **no embedding** (it is excluded from retrieval by construction, so
indexing its diluted one-vector representation would only waste space), then each
chunk as its own retrieval unit — embedded in **one batched gateway call**
(`embed_batch`) — inheriting the document's full metadata
(type/importance/tags/scopes/`sensitive`), so ABAC and the `EncryptingMemoryStore`
apply to every piece identically. A document fitting one window stores as an
ordinary memory (no linkage overhead). **Retrieval:** `passes_filters` never passes
an `is_parent` record (both the in-process and pushdown paths already funnel
through it), so parents are expansion-only; `MemoryQuery.expand_parents` (default
off) attaches the full parent document to each chunk hit as
`ScoredMemory.parent: Option<MemoryRecord>` — fail-closed through the query's own
ABAC check, and a dangling `parent_id` (parent deleted) is skipped silently.
`compress` excludes parents *and* chunks from compaction candidates outright —
consolidating one half would tear the linkage. **Tiered backend:** migration
`V2__parent_linkage.sql` adds the two columns + a partial index on `parent_id`
(existing deployments fail closed with "run `apex admin migrate --target memory`"
until migrated — the MIG-A1 contract working as designed); `TieredStore::put`
skips the Qdrant upsert for parent records. **Acceptance:**
`engine::tests::a_long_document_is_split_into_linked_chunks` (parent marked +
verbatim, every chunk linked and embedded) and
`retrieval_scores_the_relevant_chunk_above_an_irrelevant_one` (a two-topic document;
the refund-topic query's top hit is the refund chunk, outscoring the office chunk
from the same document — on the default hybrid strategy against the mock provider),
plus `parent_documents_never_surface_as_direct_hits`,
`expand_parents_attaches_the_full_document` (and off-by-default),
`parent_expansion_is_abac_fail_closed`,
`a_short_document_stores_as_an_ordinary_memory`,
`compress_leaves_document_records_alone`, and 10 `chunk::tests` covering
determinism, overlap, exact partition at zero overlap, no-word-lost coverage,
over-long words, pathological overlap ≥ window, and multibyte (UTF-8) content.
Not yet surfaced: the server's `POST /api/v1/memory/records` and the CLI's
`memory put` still ingest single records only (`remember_document` is
engine-level); wiring a `chunk: true`/policy option through the API + SDKs +
`openapi.yaml` is its own follow-on slice (the response-shape addition
`ScoredMemory.parent` is invisible until a caller opts in — the server hand-builds
its record JSON, so no wire change shipped here).

## RAG-202 `[P1]` — Re-ranking stage — **DONE (2026-07-13)**

**Problem.** Hybrid retrieval is RRF + a linear weighted score
(`engine.rs:234-243,331-407`); no cross-encoder/LLM reranker; `RRF_K` hardcoded 60
(`engine.rs:16`). (PRD-004 R-C.2; audit High.)

**Change.** Add an optional reranking stage after fusion (a `Reranker` trait: LLM- or
cross-encoder-backed) applied to the top-N candidates; make `RRF_K` configurable.

**Acceptance criteria.** A test asserts the reranker reorders a fused candidate list
and that it's off by default (opt-in), preserving current behavior.

**Files.** `crates/apex-memory/src/engine.rs` + new `rerank.rs`. **Size.** L.
**Depends on:** none.

**Implementation notes (2026-07-13).** New `apex-memory::rerank` module: a
`Reranker` trait (`rerank(query, candidates) -> Vec<f32>` — **scores in `[0,1]`,
not a permutation**, so reranked relevance flows through the existing weighted
ranker with recency/importance still applied, and stays visible in each result's
`ScoreBreakdown.relevance`) plus `LlmReranker`, the gateway-backed implementation:
one chat call listing the numbered candidates, constrained via PRV-202's
`ResponseFormat::JsonSchema` to `{"scores": [...]}`, with a lenient parser
(bare array / fenced / prose-embedded JSON also accepted, since not every
provider honors the constraint; a wrong-length or non-numeric reply is a clear
`Error::Provider`, never silent misalignment; out-of-range scores clamp). A
cross-encoder implementation drops in behind the same trait later. **Engine
wiring:** `query()` was restructured into explicit stages — retrieve+fuse
(`fused_in_process`/`fused_pushdown`, both now returning `(records, relevance)`
instead of ranked results) → optional rerank → weighted rank (+ MMR) → optional
parent expansion. `MemoryEngine::with_reranker(Arc<dyn Reranker>)` is the opt-in
(default `None` — behavior byte-identical to before); `with_rerank_top_n`
(default 20, never below the query's `limit`) caps how many fused candidates are
re-scored, the rest keeping their fused scores; a reranker failure or
wrong-shaped response **degrades to the fused order with a warning** rather than
failing the query (availability over quality — the same stance as the gateway's
semantic-cache degradation). Reranking runs *after* ABAC/metadata filtering, so
no protected content ever reaches the reranker for a caller who couldn't see it
anyway. **`RRF_K` configurable:** `reciprocal_rank_fusion` takes `k` as a
parameter, threaded from a new `MemoryEngine.rrf_k` field
(`with_rrf_k`, default 60 — previously a hardcoded const), used by both the
in-process and pushdown hybrid paths. **Acceptance:**
`engine::tests::reranker_reorders_the_fused_candidates` (a scripted reranker
inverts the keyword-fused order and the breakdown reports the reranked score) and
`without_a_reranker_the_fused_order_stands` (off by default), plus
`a_failing_reranker_degrades_to_the_fused_order`,
`only_the_fused_top_n_reaches_the_reranker`, `rrf_k_changes_the_fusion_ratio`
(smaller k weights top ranks more heavily), and 6 `rerank::tests` covering
schema-shaped/bare/fenced parsing, clamping, length-mismatch and garbage errors,
the empty-candidates short-circuit, and that the outbound request carries the
JSON-schema constraint + numbered candidates. Not yet surfaced: the server's
`memory:query` route and the CLI construct their engines without a reranker —
wiring an opt-in (e.g. `APEX_MEMORY_RERANK=llm`) is a follow-on slice, as is a
real cross-encoder backend.

## RAG-203 `[P1]` — Semantic-cache key + embedding-model id — **DONE (2026-07-13)**

**Problem.** The canonical text embedded for lookup is only the User turns
(`crates/apex-provider/src/gateway.rs:589-597`); system prompt + tools are excluded and
`param_key` guards only model+temperature (`:601-603`), so same user text + different
system/tools → wrong-context hit. Entries record no embedding-model id
(`resilience.rs:444-449`); a changed/mixed embedding model yields silently wrong
similarities (mismatched dims → cosine 0.0, `embeddings.rs:45`). (PRD-004 R-C.3; audit
High.)

**Change.** Include system prompt + tool specs in the canonical key; stamp the
embedding-model id on every `SemanticEntry` and skip/evict entries whose model id
doesn't match the current one.

**Acceptance criteria.** Tests: same user text + different system prompt does **not**
hit; an entry from a different embedding model is not served.

**Files.** `crates/apex-provider/src/{gateway.rs,resilience.rs}`. **Size.** M.
**Depends on:** none.

**Implementation notes (2026-07-13).** **Context compatibility:** the embedded
canonical text deliberately stays *user turns only* (that's the similarity
signal — what the user asked); the system prompt and the serialized tool specs
joined `param_key` instead, where compatibility is enforced *exactly* rather
than diluted into embedding similarity (a similarity threshold could never
guarantee the acceptance criterion; a key comparison does). The system/tools
text is embedded in the key verbatim, not hashed — exact, dependency-free, and
stable across processes/builds, which a `DefaultHasher` digest is not
guaranteed to be, and the Qdrant backend shares these keys across a fleet.
**Embedding-model stamping:** `SemanticEntry` gained `embedding_model`, and
the `SemanticCacheStore` trait's `lookup`/`store` both take the current
embedding-model id — `Gateway::chat` resolves it once
(`resolve_embedding_model`) and threads it through embed/lookup/store.
`InMemorySemanticCache` **skips** (not evicts) mismatched entries — they age
out via TTL, and skipping stays correct through a rolling deploy where a fleet
briefly mixes models; `QdrantSemanticCache` filters server-side (an
`embedding_model` payload field + a second `must` clause) and includes the
model id in its deterministic point id. **Tests:** the pre-existing
`semantic_cache_hits_on_meaning_match_after_exact_miss` actually *encoded the
bug* (different system prompts → expected hit) — rewritten to vary
`max_tokens` (the remaining exact-key field deliberately not part of param
compatibility), and `semantic_cache_threshold_gates_hits` likewise (it would
otherwise have passed for the wrong reason post-fix). New:
`semantic_cache_does_not_cross_system_prompts` (acceptance half 1),
`semantic_cache_does_not_cross_tool_specs`,
`semantic_cache_is_not_shared_across_embedding_models` (acceptance half 2,
end-to-end: two gateways sharing one store via a delegating wrapper, one
provider renamed so it resolves a different embedding model — the mock
embedder returns the identical vector for the identical text, so only the
model stamp separates them, and the cross-lookup correctly misses), plus the
store-level `resilience::tests::semantic_entry_from_a_different_embedding_model_is_not_served`.
The capability-gated live Qdrant test
(`tests/semantic_cache_qdrant.rs`) gained a cross-model assertion and the new
signatures; `--features qdrant` clippy-clean. `docs/05-llm-gateway/caching.md`
(→1.1.0) documents both rules in §4. Note: `cache_key` (exact cache) already
serialized full messages + tools, so the exact cache never had this bug — this
was purely a semantic-path fix.

## RAG-204 `[P2]` — BM25/TF-IDF keyword parity — **DONE (2026-07-13)**

**Problem.** In-process keyword relevance is unnormalized set-overlap of alphanumeric
tokens (`engine.rs:302-317,468-474`) — no BM25/TF-IDF/stemming — while the Postgres
pushdown path uses real FTS, so quality differs by backend. (PRD-004 R-C.4; audit Med.)

**Change.** Implement BM25 (or TF-IDF) + light stemming for the in-process keyword
branch to match the FTS backend's ranking character.

**Acceptance criteria.** A test asserts BM25 ranks a term-frequency-relevant doc above
a single-mention doc; parity smoke vs the FTS path on a shared fixture.

**Files.** `crates/apex-memory/src/engine.rs`. **Size.** M. **Depends on:** none.

**Implementation notes (2026-07-13).** `keyword_relevance` is now **BM25 over
stemmed tokens** (`k1 = 1.2`, `b = 0.75`, Lucene's non-negative
`ln(1 + (N − df + 0.5)/(df + 0.5))` IDF variant, computed over the candidate
set itself — the same corpus the scores are compared within), still normalized
to `[0,1]` by the best score so the fusion/ranker contract is unchanged. What
the old set-overlap scorer couldn't express and now works: term *frequency*
(saturating via `k1`), rare-vs-ubiquitous term weighting (IDF), document-length
normalization (`b`), and morphological matching — the old `tokenize` set became
`tokens` (order/frequency-preserving `Vec`) over a new `stem()`, a light
English suffix-stripper (≈ Porter step 1: `-ies`→`-y`, `-sses`→`-ss`,
`-ing`/`-ed`/`-es`/`-s` with minimum-stem-length and `-ss`/`-us`/`-is` guards
so "ring"/"pass"/"status" survive; at most one rule fires; approximate by
design — a miss degrades to the unstemmed token). Pure and deterministic
throughout, per the house rule. **Acceptance:**
`engine::tests::bm25_ranks_term_frequency_above_a_single_mention` (the heavy
doc is seeded *second*, so the old scorer's tie + id-ascending tiebreak would
pick the wrong one — only real tf scoring passes),
`bm25_weights_a_rare_term_above_a_ubiquitous_one` (each doc matches exactly
one query term; only IDF separates them),
`stemming_matches_morphological_variants` ("refunds" query finds a "refund"
doc with positive relevance — the old scorer scored zero overlap), a 13-case
`stem()` unit table, and empty-query/empty-corpus guards. **Parity smoke:**
`tests/tiered_backend.rs::in_process_bm25_agrees_with_postgres_fts_on_the_top_result`
runs the identical fixture through Postgres `ts_rank` and the in-process
keyword branch and asserts they agree on the top result (the
term-frequency-heavy doc) — capability-gated like the rest of that file, so it
executes for real in CI's service-container job (note: `plainto_tsquery` ANDs
terms, so FTS also drops the partial-match doc entirely; top-1 agreement is the
deliberately scoped claim). All pre-existing keyword/hybrid tests
(engine, chunking acceptance, RAG-bench) pass unchanged on BM25 scoring.

## RAG-205 `[P2]` — Real timestamps + range/time metadata filters — **DONE (2026-07-13)**

**Problem.** `seq` is an insertion counter, not a timestamp
(`crates/apex-memory/src/record.rs:66-68`), so "recency" (`engine.rs:459-465`) shifts
as records are added and is incomparable across namespaces; filters are tag-any +
min-importance + ABAC only (`engine.rs:349-353`). (PRD-004 R-C.5; audit Low.)

**Change.** Store a real creation timestamp (supplied at the boundary per the
clock-free-core rule); add time-range and numeric-range metadata filters.

**Acceptance criteria.** A test asserts recency uses wall-clock age and a time-range
filter excludes out-of-window records.

**Files.** `crates/apex-memory/src/{record.rs,engine.rs}`. **Size.** M.
**Depends on:** none.

**Implementation notes (2026-07-13).** **Timestamps:** `MemoryRecord` gained
`created_ms: u64` (`#[serde(default)]` → 0 for legacy records), stamped at
ingestion by the engine from a new injected `Clock` trait (`clock.rs`:
`SystemClock` default, `ManualClock` for deterministic tests,
`MemoryEngine::with_clock` — the same boundary-injection pattern as
`apex-workflow`'s clock, so core ranking stays a pure function of its inputs;
`remember_document` reads the clock once so a parent and all its chunks share
one creation instant). **Recency:** ranking now decays by real wall-clock age
— `exp(-age_ms / half_life_ms)` with `MemoryType::half_life_ms()` finally
implementing [ranking §4](../../06-memory-engine/ranking.md)'s actual table
(Conversation 2 *days*, Workflow 14, Episodic 90; the old code had repurposed
those numbers as "sequence units"). The seq-distance proxy survives only as
the fallback for legacy records with `created_ms == 0`, so a pre-existing
store keeps ranking sensibly instead of every old record decaying to ~0;
`rank()` takes `now_ms`, read once per query at the boundary. **Filters:**
`MemoryQuery` gained `created_after`/`created_before` (epoch ms, both
inclusive) and `max_importance` (completing the numeric range on the record's
one numeric metadata field — records carry no arbitrary numeric metadata map,
so "numeric-range" is deliberately scoped to importance). A legacy record is
excluded whenever either time bound is set: an unknown creation time cannot be
placed inside a window (fail-closed), documented on the field. **Tiered
backend:** migration `V3__created_ms.sql` (+ `put`/`row_to_record` column
wiring). **Acceptance:**
`engine::tests::recency_uses_wall_clock_age` (a `ManualClock` advanced exactly
one half-life → `breakdown.recency ≈ e⁻¹`, where the old seq proxy would read
1.0 since the sole record is also the newest; advancing another half-life
decays the *same* record to `e⁻²` — recency is a function of query-time clock,
not insertions) and `time_range_filter_excludes_out_of_window_records`
(records at t=1000/2000/3000 + a timestamp-less legacy record: a window keeps
exactly the middle one, a lower bound alone keeps two and never the legacy
record, bounds are inclusive), plus
`legacy_records_fall_back_to_sequence_decay` and
`max_importance_completes_the_numeric_range_filter` (incl. an empty
min>max band matching nothing). `ranking.md` →1.1.0 records that §4 is now
implemented as written. Not yet surfaced: the server's `memory:query`
route/CLI don't expose the new filters (`MemoryQuery` fields are
engine-level), and `record_json` doesn't return `created_ms` — API/SDK wiring
is a follow-on, consistent with the rest of WS-C. **This closes WS-C
(RAG-201..205) entirely.**

---

# WS-D — Evaluation

## EVL-201 `[P1]` — LLM-as-judge + semantic scoring — **DONE (2026-07-13)**

**Problem.** `score` supports only `contains`/`contains_all`/`equals`
(`crates/apex-eval/src/score.rs:24-72`); no LLM-as-judge, semantic similarity, or
rubric scoring. (PRD-004 R-D.1; audit High.)

**Change.** Add a judge/`Scorer` abstraction: an LLM-as-judge scorer (rubric prompt →
graded score) and a semantic-similarity scorer, alongside the existing exact
matchers.

**Acceptance criteria.** A test asserts the LLM-judge scorer grades a
semantically-correct-but-non-substring answer as passing where `contains` would fail
(against a scripted judge provider for determinism).

**Files.** `crates/apex-eval/src/score.rs` + new `judge.rs`. **Size.** M.
**Depends on:** none.

**Implementation notes (2026-07-13).** **Expectations:** the `Expectation` one-of
struct gained two model-backed variants alongside the three exact matchers —
`judge: { rubric, min_score }` (default 0.7) and `similar_to: { text, threshold }`
(default 0.8) — validated at load time like the rest (non-empty rubric/text,
scores in `[0,1]`, still exactly-one-of-five); the `Eq` derives on
`Expectation`/`Fixture`/`EvalSuite`/the compare types dropped to `PartialEq`
(the new specs carry `f32`). **New `judge.rs`:** a `Judge` trait
(`grade(input, rubric, actual) -> JudgeVerdict { score, reasoning }`) with
`LlmJudge` — one gateway chat call, PRV-202 JSON-schema-constrained to
`{"score", "reasoning"}`, leniently parsed (fenced/prose-wrapped JSON accepted;
a missing/non-numeric score is a clear error, out-of-range clamps) — plus
`SemanticScorer` (one batched embed call → embedding cosine vs threshold), and
the **`Scorer` dispatcher**: exact matchers still route through the pure
`score()` (its determinism guarantee untouched), `judge`/`similar_to` call out.
**Fail-closed throughout**, deliberately *unlike* the memory reranker's
degrade-to-fused-order: a grading failure (judge unreachable, unparseable
verdict, or no judge/embeddings configured on the scorer) fails the case with
a clear detail — there is no "previous order" to fall back to when the score
*is* the product. **Runner:** `run_suite_scored(…, &Scorer)` is the opt-in;
plain `run_suite` delegates with `Scorer::exact_only()`, so a judge call —
which costs real tokens — is always an explicit choice, never a silent side
effect of running a suite. **Acceptance:**
`tests/llm_judge_scoring.rs::judge_passes_a_paraphrased_answer_that_contains_would_fail`
— end to end through the real `run_agent` loop, an agent answering "a full
month" fails `contains: 30 days` (pass rate 0.0) and passes the rubric-judged
suite (1.0) via a **scripted judge provider on its own separate gateway**
(judging with the model that produced the answers is a known bias, noted on
`LlmJudge::new`); the scripted judge keys off the answer text embedded in the
judge prompt, so a wrong answer would genuinely fail. Plus
`plain_run_suite_fails_judged_cases_closed`,
`judged_scoring_is_reproducible_against_a_deterministic_judge` (the
byte-identical-report claim extends to model-backed scoring when the judge is
deterministic — a *live* judge's variance stays FUT-006's open ADR question,
stated in `judge.rs`'s module docs), and 8 `judge::tests` unit tests
(min_score gating, wire shape carrying rubric/input/answer + the JSON-schema
constraint, lenient-but-never-silent verdict parsing, orthogonal-vs-close
similarity via a scripted embedder, both missing-configuration paths) + 4 new
`fixture::tests`. Not yet: wiring a judge into the CI eval gate's suites
(EVL-202's territory) or the CLI.

## EVL-202 `[P1]` — Turn `apex-eval` into a regression gate — **DONE (2026-07-13)**

**Problem.** No baselines, thresholds, variance measurement, telemetry, or trend
comparison — explicitly a prototype (`crates/apex-eval/src/lib.rs:19-26`). (PRD-004
R-D.2; audit High.)

**Change.** Add golden-baseline reports, pass-rate thresholds, repeat-N variance, and
a CI step that persists eval scores as an artifact and fails on regression vs a
committed baseline (extending the existing CI eval step).

**Acceptance criteria.** A CI-runnable command fails when the pass rate drops below the
baseline threshold and passes when it meets it; variance-over-N is reported.

**Files.** `crates/apex-eval/src/*`, `.github/workflows/ci.yml`. **Size.** L.
**Depends on:** PRV-101 (cost in reports), EVL-201.

**Implementation notes (2026-07-13).** New `gate.rs`. **Golden baselines:**
`Baseline { suite, min_pass_rate, cases: BTreeMap<id, bool> }` — a committed
JSON golden file (`BTreeMap` so it serializes stably and diffs cleanly), with
`from_report` (snapshot), `load`/`save`/`from_json` (fail-closed on malformed
JSON or an out-of-range threshold). **The gate:** `check(report, baseline) ->
GateResult { passed, violations, notes }` — pure. Violations (each one fails):
wrong-suite baseline, pass rate below threshold, a baseline-passing case now
failing (**regression**, named with its detail), or a baseline case missing
from the report (a deleted fixture must not silently shrink coverage).
Notes (never a failure): an improved case or a new ungated case — both
prompting a baseline refresh. Notably, a per-case regression fails the gate
*even when the aggregate rate still meets the threshold* (proven by
`a_per_case_regression_fails_even_when_the_rate_threshold_is_met` — one case
flipping each way leaves the rate unchanged; the old rate-only idea would have
passed it). **Variance:** `run_suite_repeated(n, …)` + `VarianceReport`
(per-run pass rates, mean/min/max, and `distinct_reports` — the count of
byte-distinct serialized reports, so *any* nondeterminism is a visible number,
not an invisible flake). **Committed fixtures:** `suites/capital-facts.yaml`
(3 cases, one a judge-graded paraphrase so the gate exercises EVL-201's path
end to end) + `baselines/capital-facts.json` (min_pass_rate 1.0). **The
CI-runnable command:** `cargo test -p apex-eval --test regression_gate` —
`committed_suite_meets_the_committed_baseline_with_zero_variance` (pass
direction + variance-over-3 asserting `distinct_reports == 1`) and
`the_gate_fails_a_regressed_run_against_the_same_baseline` (fail direction:
the identical gate against a provider regressed on one case fails, naming
both the rate violation and "`japan` regressed") — so a green CI run proves
the gate *mechanism* is alive in both directions, not merely that nothing
changed; plus `committed_baseline_and_suite_agree_on_the_case_set` (a drift
tripwire between the two committed files). **Artifact persistence:** the gate
test writes `report.json`/`variance.json`/`gate.json` into
`APEX_EVAL_ARTIFACT_DIR` when set; CI's eval step (renamed "Eval regression
gate (FUT-006 / EVL-202)") sets it and a new `actions/upload-artifact` step
(`if: always()`) publishes the directory as the `eval-report` artifact —
verified locally by running the test with the env var set and inspecting the
three JSON files. **Refresh flow:** `APEX_EVAL_UPDATE_BASELINE=1 cargo test
-p apex-eval --test regression_gate` rewrites the committed golden file from
the current run (then still gates against it — a fresh snapshot must gate
clean). The CI step also now runs `llm_judge_scoring.rs` explicitly alongside
the pre-existing suites. 9 `gate::tests` unit tests cover both directions,
the vanished/new/improved cases, wrong-suite rejection, JSON round-trip +
fail-closed parsing, and zero-vs-flaky variance. Cost rides in every
persisted report via `CaseResult.usage`/`EvalReport.usage` (PRV-101's
accounting — already present, now persisted per CI run). Deferred: trend
comparison across historical artifacts (needs storage beyond per-run
artifacts) and telemetry — EVL-203 (below) covers the RAG/max_steps eval path.

## EVL-203 `[P2]` — Evaluate the RAG path + `max_steps` + retrieval metrics — **DONE (2026-07-13)**

**Problem.** `run_suite` calls `run_agent` with a bare `RunOptions::new`, ignoring
memory grounding and `spec.max_steps` (`crates/apex-eval/src/runner.rs:27-29`); no
recall@k/nDCG/MRR retriever harness. (PRD-004 R-D.3; audit High/Med.)

**Change.** Add a `run_agent_with_memory` eval path and honor manifest `max_steps`
(via AIC-103); add a retrieval-metrics harness (recall@k/nDCG/MRR) over labeled
fixtures.

**Acceptance criteria.** A test grades a RAG fixture and computes recall@k against a
labeled relevant set.

**Files.** `crates/apex-eval/src/runner.rs` + new retrieval-eval module. **Size.** M.
**Depends on:** AIC-103, RAG-201.

**Implementation notes (2026-07-13).** `run_suite_with_memory` (new, `runner.rs`)
drives `apex_agent::run_agent_with_memory` instead of the bare `run_agent`, so a
suite grades the retrieval-grounded agent a deployment actually runs; both
entry points now funnel through a shared `run_cases` so `spec.max_steps` is
honored on every runner path (AIC-103 already applies it inside the loop
itself — this just proves it end to end at the harness level, via
`the_eval_runner_honors_the_manifest_step_budget`, a tool-hungry scripted
provider capped at 2 steps). New `retrieval.rs` module: `RankedRetriever`
trait (`rank(query) -> Vec<id>`), a labeled `RetrievalSuite`/`RetrievalCase`
YAML fixture (`relevant: [ids]`, per-suite `k`), and pure metric functions
(`recall_at_k`, `reciprocal_rank`, `ndcg_at_k` with an ideal-DCG normalizer
that accounts for `k` smaller than the relevant set) aggregated by
`evaluate_retrieval` into a `RetrievalReport` (per-case + mean recall/MRR).
Proven against the **real** `apex-memory` engine (not a mock), via a
dev-only dependency on `apex-memory` (the library spine stays memory-free —
same stance as the CLI owning its own engine adapter): `tests/rag_eval.rs`
seeds a real `MemoryEngine`/`InMemoryStore` with two refund facts + two
distractors, and drives both halves — `memory_grounded_suite_passes_where_
the_memoryless_run_fails` (the memoryless path scores 0%, the
`EngineRetriever`-grounded RAG path scores 100%, over a `GroundedProvider`
that only answers correctly when its prompt actually contains the retrieved
fact) and `retrieval_metrics_grade_the_real_engine_against_labeled_fixtures`
(BM25 puts both refund docs in the top 2, so recall@2/MRR/nDCG are all
exact 1.0; a harder single-relevant-doc case proves a doc found-but-not-
ranked-first case yields `0 < MRR < 1` and `0 < nDCG < 1`, not a degenerate
0/1). All 3 `rag_eval.rs` tests + 5 new `retrieval::tests` unit tests pass;
CI's eval step now also runs `rag_eval.rs` explicitly (`ci.yml`), and
`Cargo.toml`'s crate description was updated to drop the stale "not yet
wired into CI" framing EVL-201/202 had already made untrue. This closes out
**WS-D (Evaluation) entirely** — EVL-201/202/203 all done.

---

# WS-G — Multi-Node Quotas

## SRV-201 `[P1]` — Distributed rate limiting — **DONE (2026-07-14)**

**Problem.** Token buckets live in a `Mutex<HashMap>` in `AppState`
(`crates/apex-server/src/rate_limit.rs:29-34,57-81`); N nodes each grant the full
budget. (PRD-004 R-G.5; audit High.)

**Change.** Back the limiter with a shared store (Redis, reusing the existing
`redis` feature pattern) so a fleet enforces one budget; fall back to in-process for
single-node.

**Acceptance criteria.** A gated test asserts two limiter instances over a shared
store enforce a combined budget, not 2×.

**Files.** `crates/apex-server/src/rate_limit.rs`. **Size.** M. **Depends on:** none.

**Implementation notes (2026-07-14).** `RateLimiter` now holds the in-process
`LocalBuckets` (today's logic, unchanged) *plus* an optional Redis-shared
backend behind a new `apex-server` `redis` cargo feature (same version/shape as
`apex-provider`'s; `apex-cli` forwards it so the embedded `apex dev` picks it
up), selected at runtime by `APEX_RATE_LIMIT_REDIS_URL` via
`RateLimiter::from_env` — a dedicated variable rather than reusing
`APEX_REDIS_URL`, so CI's service-container job (which sets the latter for the
breaker tests) doesn't silently flip every server test onto shared limiting.
The shared path keeps **token-bucket semantics identical to the local one**
(not a fixed-window approximation): the refill-then-take runs as one atomic
Lua `EVAL` inside Redis (state = a per-key hash `{t, ts}`; `ts` never moves
backwards, so a fleet node with a slower clock can't rewind a bucket into
double refill; token counts travel as strings because Lua→Redis integer
replies truncate fractions), keys are namespaced per tier
(`apex:rl:{standard|sensitive}:{key}`) so the two tiers never share a bucket,
and every touch re-arms a `PEXPIRE` at worst-case-full-refill + slack — the
shared-store equivalent of the local sweep. **Failure mode: degrade to
per-node limiting, never to unlimited** — the connection is dialed lazily,
cached, wrapped in a 1 s budget (a rate-limit check sits on every request; a
slow Redis must not become a global stall), and dropped on any error so the
next check re-dials; while Redis is unreachable every check falls back to the
in-process bucket with a warning. Setting the env var on a binary built
without the feature logs a loud error instead of silently running per-node.
Proven three ways: offline unit tests (existing bucket tests now async, plus
`unreachable_redis_degrades_to_local_limiting_not_unlimited`, which needs no
live Redis); capability-gated integration tests inline in `rate_limit.rs`
(`RateLimiter` is deliberately `pub(crate)`, so they can't live under
`tests/` — same reasoning as `lib.rs`'s inline suite) covering the acceptance
criterion (`two_limiter_instances_enforce_one_combined_budget`: 4 admits
alternating across two limiter instances over one prefix, 5th rejected on
*both* — one budget, not 2×), per-key/per-tier isolation, and continuous
refill; and the full 129-test server suite run `--features redis` against a
real Redis 7 container locally — all green. CI's service-container job gained
`run_gated cargo test -p apex-server --features redis --lib rate_limit`.

## SRV-202 `[P1]` — Per-tenant token quotas; enforce/remove dead dimensions — **DONE (2026-07-14)**

**Problem.** `QuotaLimits` declares `tool_executions_per_minute` and `memory_records`
but `admit_run` enforces only concurrent runs + daily USD cost
(`crates/apex-tenancy/src/model.rs:125-138`, `apex-server/src/tenancy.rs:553,560`);
cost is USD-only with no token budget, and the rate limiter is per-principal not
per-tenant. (PRD-004 R-G.6; audit Med.)

**Change.** Add a per-tenant token budget; enforce (or remove) the two dead quota
dimensions; add a per-tenant rate tier.

**Acceptance criteria.** Tests assert a token budget blocks at threshold and that the
previously-dead dimensions are either enforced or gone.

**Files.** `crates/apex-tenancy/src/{model.rs,quota.rs}`, `apex-server/src/tenancy.rs`.
**Size.** M. **Depends on:** PRV-101 (real token accounting).

**Implementation notes (2026-07-14).** Three parts. **Token budget:**
`QuotaLimits.llm_tokens_per_day` (+ pure `check_llm_tokens`) — the
vendor-bill-independent twin of the cost budget (a local model is $0/token but
still burns capacity). The server's `QuotaTracker` accumulates tokens alongside
cost (`record_run_cost` → `record_run_usage(cost, tokens)`;
`AgentResolver::record` in `apex-runtime` now takes the full
`apex_common::Usage` so every platform's hook sees tokens without a second
method), `admit_run` checks it with the same observe-then-enforce boundary as
cost (admitted while within budget; the *next* run after crossing is refused
`429`), and the accumulator's persisted entries grew `[day, usd]` →
`[day, usd, tokens]` with an untagged-enum loader that still accepts the old
shape (tokens default 0) — an upgrade must not treat the old file as corrupt
and silently reset every project's spend to $0, the exact DUR-404 failure mode
(proven by `pre_token_quota_file_loads_with_spend_preserved`). **Dead
dimensions: removed, not enforced** — `tool_executions_per_minute` (tool
executions happen inside the agent loop where no per-project window tracker
exists; request-level abuse is the rate limiter's job) and `memory_records`
(records are *tenant*-namespaced, quotas are *project*-scoped — "a project's
record count" was never well-defined). Dead config an operator could set and
reasonably believe was protecting them is worse than an honest absence; a
stored quota still carrying the old fields deserializes fine (serde ignores
unknown fields, proven by `legacy_quota_json_with_removed_fields_still_loads`).
Neither field was ever in `openapi.yaml` or the SDKs — only the dashboard's
settings form, updated in lockstep (`llm_tokens_per_day` input added, dead
inputs removed; `openapi.yaml`'s quota PATCH schema and
`docs/09-api/projects.md` §5 updated too). **Per-tenant rate tier:**
`APEX_RATE_LIMIT_TENANT_PER_MIN` (opt-in; unset = no tier, exactly the old
behavior) enables a third `RateLimiter` keyed `tenant:{X-Apex-Tenant}` (falling
back to `default`, so anonymous traffic is bounded too), checked in the same
`enforce` middleware after the per-principal bucket so the tenant budget is
only consumed by requests the caller's own budget admitted — and shareable
across a fleet via SRV-201's Redis path like the other tiers. Documented
caveat (same spirit as the existing `X-Forwarded-For` note): the tenant header
is client-asserted and only *authorization*-checked downstream, so an
authenticated caller spoofing another tenant's id burns that tenant's rate
budget even though the request itself 403s at `tenant_authorize`. Proven by
`quota.rs::enforces_token_budget_at_the_threshold` (pure, exact boundary),
`tenancy.rs::token_budget_blocks_admission_at_threshold` (admission refuses
`429` with `llm_tokens_per_day` named once usage crosses the limit; other
projects unaffected), the extended PRV-101/DUR-404/RUN-202 accumulator tests
(tokens accumulate, survive restart, and land from real workflow sub-agent
runs), and `lib.rs::tenant_rate_tier_is_shared_across_principals_and_isolated_
by_tenant` (two principals under one tenant exhaust the shared budget, a third
429s, another tenant is untouched).

## SRV-203 `[P2]` — Tenant-configurable daily-cost reset boundary — **DONE (2026-07-14)**

**Problem.** `current_day()` = epoch-seconds/86400, so budgets reset at 00:00 UTC for
everyone (`crates/apex-server/src/tenancy.rs:497-502`). (PRD-004 R-G.7; audit Med.)

**Change.** Make the reset boundary tenant-configurable (timezone/offset), read only
at the server boundary per the clock-free-core rule.

**Acceptance criteria.** A test asserts a tenant with a non-UTC offset resets at its
local midnight.

**Files.** `crates/apex-server/src/tenancy.rs`, tenancy model. **Size.** S.
**Depends on:** none.

**Implementation notes (2026-07-14).**
`QuotaLimits.day_reset_offset_minutes: Option<i32>` (minutes east of UTC —
minutes, not hours, because real timezones include half- and quarter-hour
offsets like IST +330; `None` = UTC midnight, the exact pre-SRV-203 behavior)
rides the same quota record the PATCH route/dashboard already edit, so "the
tenant's timezone" is configured where the daily budgets it governs live. The
day computation split into a **pure** `day_bucket(epoch_secs, offset_minutes)`
(`div_euclid` for correct negative-offset flooring; clamped to ±24 h so a
garbage stored value can't skew a window by more than a day) and a thin
`current_day_with_offset` that reads the wall clock — time still enters only
at the server boundary, and the boundary math is deterministically testable.
`admit_run` resolves the bucket from the quota it already loaded;
`record_run_usage` now takes the `TenancyStore` and looks the quota up itself,
so **recording and admission always agree on which day usage lands in** —
that agreement, not the offset arithmetic, is the actual correctness property
(a mismatch would leak usage across the tenant's midnight or wrongly zero it).
Accumulator entries recorded before an offset change simply belong to a
different bucket and read as zero — same effect as any day rollover, no
migration needed. Proven by
`day_bucket_flips_at_the_configured_local_midnight` (the acceptance test:
exact second-level flips at 00:00 IST/+330 and 00:00 EST/−300 while the UTC
bucket disagrees, plus the clamp) and
`offset_quota_records_and_enforces_under_its_own_day_bucket` (wiring: at any
wall-clock time the test picks a ±12 h offset guaranteed to differ from UTC's
current bucket, records usage, asserts nothing landed under the UTC bucket
and everything under the offset bucket, and that admission — reading the same
bucket — refuses the crossed token budget). `openapi.yaml`'s quota PATCH
schema, the dashboard's quota form/types, and `projects.md` §5 updated in
lockstep. This closes **WS-G (Multi-Node Quotas) entirely** — SRV-201/202/203
all done.

---

# WS-A / WS-runtime — Loop recovery, streaming, `ai` activity

## AIC-201 `[P1]` — Step-error recovery + forced final answer — **DONE (2026-07-13)**

**Problem.** A provider/stream error aborts the whole run via `?`
(`crates/apex-agent/src/runtime.rs:247`); on budget exhaustion with pending tool
calls the loop hard-errors with no answer (`:273-278`). (PRD-004 R-A.4; audit Med.)

**Change.** Retry a recoverable model-step error; on the last step, re-call the model
with tools disabled to force a final answer instead of erroring.

**Acceptance criteria.** Tests: a transient step error retries and completes; a run at
the step cap returns a tool-less final answer, not `Error::Runtime`.

**Files.** `crates/apex-agent/src/runtime.rs`. **Size.** M. **Depends on:** none.

**Implementation notes (2026-07-13).** Both halves live in `run_agent_inner`
(`crates/apex-agent/src/runtime.rs`), nothing else on the spine changed.
**Step retry:** the `stream_chat` call is wrapped in a bounded re-issue loop —
a step that fails with `Error::Provider { .. }` (the same transient
classification the gateway's own `is_transient` uses) is re-issued up to
`RunOptions::step_retries` times (`with_step_retries`, default 2; `0`
restores the old abort-on-first-error behavior); any other error class
(`Invalid`/`Config`/…) still aborts immediately. This deliberately targets
the failure the gateway's resilience stack *can't* absorb — a stream that
errors or truncates **mid-flight** (per-call retry doesn't apply to streams;
establishment failures were already covered by gateway failover/breaker) —
and there is **no backoff at this layer**: each re-issue passes back through
the gateway's full jittered-retry/failover/breaker pipeline (PRV-205), which
owns pacing, so sleeping here would double-sleep. Deltas emitted before a
mid-stream failure may be re-emitted by the retried attempt (display-only;
the history fed back comes from the terminal `Done`). **Forced final
answer:** the *last budgeted* step (`step + 1 == max_steps`, when the agent
has tools) advertises **no tools** and appends a one-line system instruction
("step budget exhausted… answer from what you have") to the request copy
only — the model can't request work there's no budget left to execute, so a
well-behaved model answers and the run returns `Ok` with the gathered
context intact, within `max_steps` model calls (interpretation (a): the
forced call *is* the last step, not an extra one, so a `max_steps` budget
still means exactly that many model calls). `max_steps == 0` (no call
allowed at all — nothing to force) and a pathological provider that returns
tool calls despite none being advertised both still end in the pre-existing
`Error::Runtime("did not finish within N steps")`, so the budget guard
holds. Proven by four new `runtime.rs` tests —
`transient_mid_stream_error_is_retried_and_the_run_completes` (a scripted
provider whose stream yields a delta then an `Err` for exactly the default
retry budget, invisible to gateway establishment retry; 3 calls, 1 step),
`step_retries_zero_restores_abort_on_first_error`,
`permanent_mid_stream_error_is_not_retried` (an `Error::Invalid` mid-stream
fails after exactly 1 call), and
`run_at_the_step_cap_returns_a_forced_final_answer` (a tool-hungry provider
capped at 3 steps returns the forced answer with `steps == 3`, and the last
request it saw is asserted tool-less and carrying the injected note) — plus
an updated comment on `tool_loop.rs::run_loop_terminates_on_step_budget`,
which now documents that it exercises the pathological-provider fallback.

## AIC-202 `[P2]` — Richer streaming events — **DONE (2026-07-13)**

**Problem.** Only text deltas are emitted (`runtime.rs:302-305`); tool-call-argument
streaming and any reasoning/thinking channel aren't surfaced (`events.rs:13-27`);
mistralrs has no real streaming (`provider.rs:38-46`). (PRD-004 R-A.5; audit Low.)

**Change.** Add `ToolCallDelta` and (where available) reasoning events to
`RunEventSink`; surface them in the CLI/dashboard renderers.

**Acceptance criteria.** A test asserts tool-call-argument deltas are emitted during a
streamed tool turn.

**Files.** `crates/apex-agent/src/{runtime.rs,events.rs}`, CLI/dashboard sinks.
**Size.** M. **Depends on:** none.

**Implementation notes (2026-07-13).** The channel is plumbed end to end,
provider wire → agent sink → every renderer. **Provider layer:**
`ChatStreamEvent` gained `ToolCallDelta { index, id, name, arguments }` (one
event per wire chunk; `id`/`name` always carry the values accumulated so far —
both protocols send them at the call's start — so consumers never join
fragments across events; `arguments` is the chunk's fragment only, empty on
the announcement that opens a call) and `ReasoningDelta(String)`
(display-only; never accumulated into the final message). `OpenAiProvider`'s
`StreamAccumulator::ingest` now returns the `Vec<ChatStreamEvent>` to surface
(tool-call fragments from `delta.tool_calls`, reasoning from an
OpenAI-compatible server's `delta.reasoning_content` — the DeepSeek-style
channel, since OpenAI's own o-series hides its reasoning); `AnthropicProvider`'s
`Ingested` gained matching variants (`content_block_start`/`tool_use`
announces the call, each `input_json_delta` streams its fragment,
`thinking_delta` surfaces as reasoning; `signature_delta` stays
bookkeeping-only). The terminal `Done` response is unchanged and remains the
only thing the agent loop *acts* on — the incremental channel is
display-only, so a consumer that ignores it sees exactly the old behavior.
The gateway needed no change (its stream map only touches `Done`).
**Agent layer:** `RunEvent::ToolCallDelta { index, name, arguments }` +
`RunEvent::ReasoningDelta { text }`, emitted by `stream_chat` as the events
pass through. **Renderers:** the CLI's `StreamSink` prints `targs · #0 echo
"…"` / `think · "…"` lines; the server's `ChannelSink` emits
`{"type":"tool_call_delta",...}` / `{"type":"reasoning",...}` SSE frames
(documented in `openapi.yaml`'s `agents:stream` description, with an explicit
"ignore unknown frame types" note — both SDKs' parsers already pass unknown
frames through, and the TS SDK's `AgentStreamEvent` union gained the two new
frame types); the dashboard parses both (`agent.service.ts`), coalesces
argument fragments per call into one live "composing…" console line rather
than a line per fragment, and accumulates reasoning into a dim `think` block
above the answer (`agent-studio.ts`/`.html`). Proven by
`runtime.rs::tool_call_argument_deltas_stream_through_the_sink` (the
acceptance test: a scripted streaming tool turn yields reasoning + argument
fragments in wire order through the sink, before the complete `ToolCall`
announcement), extended adapter tests
(`accumulates_streamed_tool_call_across_chunks`,
`reasoning_content_surfaces_as_a_reasoning_delta`,
`accumulates_streamed_tool_use_across_json_deltas`,
`thinking_delta_surfaces_as_reasoning`), the real-HTTP fixture tests
(`openai_stream.rs::streams_tool_call_in_final_done`,
`anthropic_messages.rs::streams_tool_use_assembled_from_partial_json` — both
now assert the live fragments too), and the dashboard's extended SSE-parsing
spec (19/19 Jasmine specs pass; `ng build` clean). Deliberately **not**
addressed from the problem statement: `MistralRsProvider` still streams via
the default chat-wrapping shim — real mistral.rs token streaming is its own
slice (the ticket's Files list never included it, and the crate is excluded
from CI as too heavy a compile).

## RUN-201 `[P2]` — `ai` activity: honor model/params + correct error class — **DONE (2026-07-13)**

**Problem.** The shared executor's `ai` activity ignores temperature/max_tokens/tools
and always uses the default fast model, and classifies every failure `Retryable`
even for permanent bad-request errors (`crates/apex-runtime/src/lib.rs:197-217`).
(PRD-004 R-A.4; audit Med.)

**Change.** Let an `ai` step pin a model and pass temperature/max_tokens/response_format;
classify permanent (validation/bad-request) errors as non-retryable.

**Acceptance criteria.** A test asserts an `ai` step honors a pinned model and that a
validation error is not retried.

**Files.** `crates/apex-runtime/src/lib.rs`. **Size.** M. **Depends on:** PRV-202.

**Implementation notes (2026-07-13).** The `ai` branch of
`PlatformActivityExecutor::execute` now reads `inputs.model` (pinned via
`Gateway::resolve_model(Some(..))` — a pin always wins; absent, the resolved
default is unchanged), `inputs.temperature`, `inputs.max_tokens`, and
`inputs.response_format` — the last deserialized directly into PRV-202's
`ResponseFormat` wire shape (`json_object`, or `{json_schema: {name, schema}}`),
with a malformed value failing **permanently before the model is ever called**
(a definition bug can't succeed on retry). Everything rides under `inputs`
because the implemented `ActivityDef` has no top-level model/param fields —
`workflow-dsl.md` §14 now documents this implemented shape alongside its
aspirational one. **Error classification** is a shared
`classify_gateway_error` helper: `Error::Provider`/`QuotaExceeded` →
`Retryable` (the provider may recover; a budget window may reset), everything
else (`Invalid`, `Config`, `Runtime`, …) → `Permanent` — previously every
failure blanket-classified `Retryable`, so a permanently malformed step burned
the workflow's whole retry budget. The helper is deliberately also applied to
the `agent` branch's `run_agent` failure (same defect shape, one line — an
agent manifest referencing an unknown tool is `Error::Config` and now fails
its activity permanently instead of retrying into the same config error;
admission rejections stay `Retryable` exactly as before, per the existing
`AgentResolver::admit` contract and its test). Proven by five new
`apex-runtime` tests against a request-recording provider:
`ai_activity_honors_pinned_model_params_and_response_format` (all four fields
asserted on the wire request),
`ai_activity_without_params_keeps_the_resolved_default_model` (back-compat),
`ai_activity_validation_error_is_permanent_not_retried` (also asserts exactly
one provider call end to end — the gateway doesn't failover a permanent error
either), `ai_activity_transient_provider_error_stays_retryable`, and
`ai_activity_malformed_response_format_is_permanent` (model never called).
Not addressed (matches the ticket's own scope): `ai` steps still can't
advertise *tools* — a tool-using step is what `agent` activities are for.

## RUN-202 `[P2]` — Sub-agent run observability + real cost — **DONE (2026-07-13)**

**Problem.** `agent` activities run with `NullSink` and record only cost
(`crates/apex-runtime/src/lib.rs:253,257`) — which is $0 for real providers until
PRV-101, making server budget enforcement a no-op. (PRD-004 R-B.1; audit Low.)

**Change.** Attach a real event sink (or a span-emitting sink) to sub-agent runs and
record the PRV-101 cost against the parent project budget.

**Acceptance criteria.** A test asserts a sub-agent activity's cost is non-zero and is
charged to the project's daily accumulator.

**Files.** `crates/apex-runtime/src/lib.rs`. **Size.** S. **Depends on:** PRV-101.

**Implementation notes (2026-07-13).** **Observability:** the `agent` branch's
`NullSink` is replaced by a `TracingSink` (in `apex-runtime`) emitting each
sub-agent run's lifecycle as structured `tracing` events under target
`apex.runtime.agent`, keyed by the owning workflow activity id + agent name —
`Start` (model/provider) and `Done` (total tokens + cost) at `info`,
memory-retrieval and tool call/result at `debug`; token-level streams
(`Delta`/`ToolCallDelta`/`ReasoningDelta`) deliberately unlogged (per-token log
lines are noise, and the OTLP `agent.run` span already carries run timing).
These flow to OTLP logs automatically via `apex-telemetry`'s existing
appender bridge — no new wiring needed. **Cost:** the accounting *plumbing*
already existed (`AgentResolver::record` → the server's
`tenancy::record_run_cost`) and PRV-101 already made `usage.cost_usd` real —
what RUN-202 adds is *proof the chain actually works end to end*, which no
test asserted: `apex-runtime`'s `agent_activity_records_a_non_zero_run_cost`
(a recording resolver sees exactly one `record` call with cost > 0 from a
provider that reports real usage) and `apex-server`'s
`agent_activity_cost_is_charged_to_the_project_accumulator` (a full
HTTP-submitted agent workflow with `X-Apex-Project` completes and the
project's daily accumulator shows a non-zero spend — read back via a new
test-only `QuotaTracker::spent_today` accessor, the read half of
`record_run_cost`). This closes **WS-A / WS-runtime entirely** —
AIC-201/202 and RUN-201/202 all done.

---

# WS-I — Guardrails & Prompt Management

## SAF-201 `[P1]` — Content-safety / moderation / PII hooks — **DONE (2026-07-14)**

**Problem.** No moderation, PII-redaction, or jailbreak checks anywhere in the agent
loop or provider layer (audit grep). (PRD-004 R-I.1; audit Med.)

**Change.** Add a `Guardrail` trait invoked on model input and output in `run_agent`
(pluggable: a moderation provider, a PII redactor, a jailbreak classifier), fail-closed
or annotate per policy.

**Acceptance criteria.** A test asserts a configured guardrail can block/redact a
flagged input and output; absent config, behavior is unchanged.

**Files.** `crates/apex-agent/src/runtime.rs` + new `guardrail.rs`. **Size.** L.
**Depends on:** none.

**Implementation notes (2026-07-14).** New `crates/apex-agent/src/guardrail.rs`:
a `Guardrail` trait (`check(stage, content) -> GuardrailDecision` —
`Allow`/`Redact(replacement)`/`Block(reason)`; `applies_to(stage)` lets a cheap
input-only filter opt out of the output stage so its mere presence doesn't
disable streaming) and a `Guardrails` ordered set attached via
`RunOptions::with_guardrail` — empty by default, so an unconfigured run
behaves exactly as before (the acceptance criterion's back-compat half is
every pre-existing runtime test passing unchanged). Applied at two points in
`run_agent`: the user turn immediately after extraction — **before retrieval**,
so a redaction also keeps PII out of the memory engine's query, and a block
costs zero model calls — and the final answer before it returns. **Fail-closed,
deliberately** (the `apex-eval` stance, not the memory-reranker degrade — a
safety control whose failure mode is "no safety" isn't one): a guardrail
*error* fails the run with a "failing closed" `Runtime` error; a *block*
surfaces as `Error::Forbidden` — permanent, so neither the gateway nor a
workflow retry loop retries content that will be refused again. **The
streaming side channel is closed too**: when any configured guardrail checks
the output stage, `stream_chat` buffers (no raw `Delta`/`ToolCallDelta`/
`ReasoningDelta` reaches the sink) and the checked final answer is emitted as
one `Delta` after the output check passes — otherwise an output guardrail
would be decorative, with the unredacted text already streamed. Three
implementations ship: `BlocklistGuardrail` (deterministic case-insensitive
deny-list whose block reason deliberately doesn't echo the matched term — the
blocklist is policy, not something to leak back one probe at a time),
`PiiRedactor` (deterministic, dependency-free email + long-digit-run
redaction — a documented light heuristic in the `HeuristicTokenizer` spirit,
not a DLP engine), and `LlmModerator` (one gateway chat call, PRV-202
JSON-schema-constrained to `{"flagged", "reason"}`, lenient about code fences
but an unparseable verdict is an error, never a silent allow; runs on its
*own* gateway with the same self-moderation-bias note as `LlmJudge`).
Proven by four run-loop acceptance tests
(`input_guardrail_blocks_before_any_model_call` — zero provider calls,
`input_guardrail_redaction_reaches_the_model_not_the_raw_pii`,
`output_guardrail_redacts_the_answer_and_buffers_streaming` — the sink sees
exactly one `Delta`, the checked text,
`output_guardrail_block_fails_the_run_without_leaking_deltas`) plus seven
`guardrail.rs` unit tests (sequential piping — a later blocklist sees the
earlier redaction; fail-closed on guardrail error; blocklist/redactor/email
heuristics; both `LlmModerator` verdict directions and its unparseable-verdict
error). Not yet surfaced in the agent manifest YAML or the server/CLI —
callers attach guardrails programmatically via `RunOptions`, the same stance
as PRV-202/PRV-204; manifest/API wiring is a follow-on.

## SAF-202 `[P2]` — Prompt template/versioning registry

**Problem.** Instructions are a raw YAML string (`crates/apex-agent/src/definition.rs:39`);
only workflow `${...}` interpolation exists — no versioned prompt registry, variables,
or A/B. (PRD-004 R-I.2; audit Med.)

**Change.** Add a prompt registry (named, versioned templates with typed variables);
agents reference a template + version; support A/B selection.

**Acceptance criteria.** A test resolves a versioned template with variables and pins a
version across runs.

**Files.** `crates/apex-agent/src/` (new `prompt.rs`). **Size.** M. **Depends on:** none.

---

# WS-L — Per-Tenant Metrics

## OBS-201 `[P1]` — Per-tenant / per-project metric labels

**Problem.** RED metrics are labeled only `route`/`method`/`status`
(`crates/apex-server/src/hardening.rs:1008-1020`); LLM cost/token metrics are labeled
by `model` only (`config.rs:385-406`) — a noisy tenant is invisible. (PRD-004 R-L.1;
audit Med.)

**Change.** Add a bounded-cardinality `tenant`/`project` label (or a separate
per-tenant aggregate) to request and LLM metrics.

**Acceptance criteria.** A test asserts a request/LLM metric carries a tenant label and
that cardinality stays bounded (a capped/hashed label set).

**Files.** `crates/apex-server/src/{hardening.rs,config.rs}`. **Size.** M.
**Depends on:** PRV-101 (real cost to label).

---

# Exit criteria (Phase 2)

1. Claude is a first-class provider; structured output and multimodal exist (PRV-201/202/204).
2. Memory chunks + reranks; the semantic cache never serves a wrong-context or
   wrong-model hit; keyword parity across backends (RAG-201..205).
3. `apex-eval` fails CI on a real quality regression and can grade RAG (EVL-201..203).
4. Multi-node rate limits + per-tenant token quotas are correct; daily windows are
   tenant-local (SRV-201..203).
5. The agent loop recovers from step errors and always returns an answer; guardrails
   can gate input/output; prompts are versioned (AIC-201, SAF-201/202).
6. Per-tenant metrics exist (OBS-201).

---

# Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-07-09 | Initial Phase-2 tickets from PRD-004 / the 2026-07-09 engineering audit (credible-AI-product P1/P2 work) |
| 1.1.0 | 2026-07-13 | PRV-201 (first-class `AnthropicProvider`) implemented and marked DONE with implementation notes |
| 1.2.0 | 2026-07-13 | PRV-202 (structured output / forced tool) implemented and marked DONE with implementation notes |
| 1.3.0 | 2026-07-13 | PRV-203 (tool-schema normalization + surfaced arg-parse errors) implemented and marked DONE with implementation notes |
| 1.4.0 | 2026-07-13 | PRV-204 (multimodal content parts) implemented and marked DONE with implementation notes |
| 1.5.0 | 2026-07-13 | PRV-205 (retry jitter + `Retry-After`) implemented and marked DONE with implementation notes — all of WS-B (Providers) is now done |
| 1.6.0 | 2026-07-13 | RAG-201 (document chunking with parent-document linkage) implemented and marked DONE with implementation notes |
| 1.7.0 | 2026-07-13 | RAG-202 (re-ranking stage) implemented and marked DONE with implementation notes |
| 1.8.0 | 2026-07-13 | RAG-203 (semantic-cache context compatibility + embedding-model stamping) implemented and marked DONE with implementation notes |
| 1.9.0 | 2026-07-13 | RAG-204 (BM25 + light stemming for the in-process keyword branch) implemented and marked DONE with implementation notes |
| 1.10.0 | 2026-07-13 | RAG-205 (real timestamps + range/time metadata filters) implemented and marked DONE with implementation notes — all of WS-C (Memory & RAG) is now done |
| 1.11.0 | 2026-07-13 | EVL-201 (LLM-as-judge + semantic scoring) implemented and marked DONE with implementation notes |
| 1.12.0 | 2026-07-13 | EVL-202 (quantified regression gate: golden baselines, thresholds, repeat-N variance, CI artifact persistence) implemented and marked DONE with implementation notes |
| 1.13.0 | 2026-07-13 | EVL-203 (RAG-path eval + retrieval metrics) implemented and marked DONE with implementation notes — all of WS-D (Evaluation) is now done (row added retroactively in 1.14.0; the version bump itself happened with EVL-203) |
| 1.14.0 | 2026-07-13 | AIC-201 (step-error recovery + forced final answer) implemented and marked DONE with implementation notes |
| 1.15.0 | 2026-07-13 | AIC-202 (richer streaming events: tool-call-argument + reasoning deltas, wire → sink → CLI/SSE/dashboard) implemented and marked DONE with implementation notes |
| 1.16.0 | 2026-07-13 | RUN-201 (`ai` activity honors model/temperature/max_tokens/response_format; gateway errors classify Retryable-vs-Permanent by kind) implemented and marked DONE with implementation notes |
| 1.17.0 | 2026-07-13 | RUN-202 (sub-agent runs get a TracingSink instead of NullSink; non-zero run cost proven to reach the project's daily accumulator end to end) implemented and marked DONE with implementation notes — all of WS-A / WS-runtime is now done |
| 1.18.0 | 2026-07-14 | SRV-201 (Redis-shared rate limiting: atomic Lua token bucket, per-tier prefixes, degrade-to-per-node on Redis failure, `APEX_RATE_LIMIT_REDIS_URL` + `redis` feature, gated combined-budget test wired into CI) implemented and marked DONE with implementation notes |
| 1.19.0 | 2026-07-14 | SRV-202 (`llm_tokens_per_day` budget with back-compat accumulator persistence; dead `tool_executions_per_minute`/`memory_records` dimensions removed; opt-in per-tenant rate tier) implemented and marked DONE with implementation notes |
| 1.20.0 | 2026-07-14 | SRV-203 (per-quota `day_reset_offset_minutes` daily-reset boundary; pure `day_bucket` math; admission/recording bucket agreement) implemented and marked DONE with implementation notes — all of WS-G (Multi-Node Quotas) is now done |
| 1.21.0 | 2026-07-14 | SAF-201 (pluggable `Guardrail` trait on input/output: block/redact, fail-closed, buffered streaming; blocklist/PII-redactor/LLM-moderator implementations) implemented and marked DONE with implementation notes |
