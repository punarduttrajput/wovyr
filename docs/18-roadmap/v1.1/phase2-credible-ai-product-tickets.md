<!--
File: docs/18-roadmap/v1.1/phase2-credible-ai-product-tickets.md
Document ID: RM-AIM-P2
-->

# Phase 2 — Credible AI Product: Implementation Tickets

**Document ID:** RM-AIM-P2
**File Path:** `docs/18-roadmap/v1.1/phase2-credible-ai-product-tickets.md`
**Version:** 1.6.0
**Status:** In progress — WS-B (PRV-201..205) fully done; RAG-201 done
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

## RAG-202 `[P1]` — Re-ranking stage

**Problem.** Hybrid retrieval is RRF + a linear weighted score
(`engine.rs:234-243,331-407`); no cross-encoder/LLM reranker; `RRF_K` hardcoded 60
(`engine.rs:16`). (PRD-004 R-C.2; audit High.)

**Change.** Add an optional reranking stage after fusion (a `Reranker` trait: LLM- or
cross-encoder-backed) applied to the top-N candidates; make `RRF_K` configurable.

**Acceptance criteria.** A test asserts the reranker reorders a fused candidate list
and that it's off by default (opt-in), preserving current behavior.

**Files.** `crates/apex-memory/src/engine.rs` + new `rerank.rs`. **Size.** L.
**Depends on:** none.

## RAG-203 `[P1]` — Semantic-cache key + embedding-model id

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

## RAG-204 `[P2]` — BM25/TF-IDF keyword parity

**Problem.** In-process keyword relevance is unnormalized set-overlap of alphanumeric
tokens (`engine.rs:302-317,468-474`) — no BM25/TF-IDF/stemming — while the Postgres
pushdown path uses real FTS, so quality differs by backend. (PRD-004 R-C.4; audit Med.)

**Change.** Implement BM25 (or TF-IDF) + light stemming for the in-process keyword
branch to match the FTS backend's ranking character.

**Acceptance criteria.** A test asserts BM25 ranks a term-frequency-relevant doc above
a single-mention doc; parity smoke vs the FTS path on a shared fixture.

**Files.** `crates/apex-memory/src/engine.rs`. **Size.** M. **Depends on:** none.

## RAG-205 `[P2]` — Real timestamps + range/time metadata filters

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

---

# WS-D — Evaluation

## EVL-201 `[P1]` — LLM-as-judge + semantic scoring

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

## EVL-202 `[P1]` — Turn `apex-eval` into a regression gate

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

## EVL-203 `[P2]` — Evaluate the RAG path + `max_steps` + retrieval metrics

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

---

# WS-G — Multi-Node Quotas

## SRV-201 `[P1]` — Distributed rate limiting

**Problem.** Token buckets live in a `Mutex<HashMap>` in `AppState`
(`crates/apex-server/src/rate_limit.rs:29-34,57-81`); N nodes each grant the full
budget. (PRD-004 R-G.5; audit High.)

**Change.** Back the limiter with a shared store (Redis, reusing the existing
`redis` feature pattern) so a fleet enforces one budget; fall back to in-process for
single-node.

**Acceptance criteria.** A gated test asserts two limiter instances over a shared
store enforce a combined budget, not 2×.

**Files.** `crates/apex-server/src/rate_limit.rs`. **Size.** M. **Depends on:** none.

## SRV-202 `[P1]` — Per-tenant token quotas; enforce/remove dead dimensions

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

## SRV-203 `[P2]` — Tenant-configurable daily-cost reset boundary

**Problem.** `current_day()` = epoch-seconds/86400, so budgets reset at 00:00 UTC for
everyone (`crates/apex-server/src/tenancy.rs:497-502`). (PRD-004 R-G.7; audit Med.)

**Change.** Make the reset boundary tenant-configurable (timezone/offset), read only
at the server boundary per the clock-free-core rule.

**Acceptance criteria.** A test asserts a tenant with a non-UTC offset resets at its
local midnight.

**Files.** `crates/apex-server/src/tenancy.rs`, tenancy model. **Size.** S.
**Depends on:** none.

---

# WS-A / WS-runtime — Loop recovery, streaming, `ai` activity

## AIC-201 `[P1]` — Step-error recovery + forced final answer

**Problem.** A provider/stream error aborts the whole run via `?`
(`crates/apex-agent/src/runtime.rs:247`); on budget exhaustion with pending tool
calls the loop hard-errors with no answer (`:273-278`). (PRD-004 R-A.4; audit Med.)

**Change.** Retry a recoverable model-step error; on the last step, re-call the model
with tools disabled to force a final answer instead of erroring.

**Acceptance criteria.** Tests: a transient step error retries and completes; a run at
the step cap returns a tool-less final answer, not `Error::Runtime`.

**Files.** `crates/apex-agent/src/runtime.rs`. **Size.** M. **Depends on:** none.

## AIC-202 `[P2]` — Richer streaming events

**Problem.** Only text deltas are emitted (`runtime.rs:302-305`); tool-call-argument
streaming and any reasoning/thinking channel aren't surfaced (`events.rs:13-27`);
mistralrs has no real streaming (`provider.rs:38-46`). (PRD-004 R-A.5; audit Low.)

**Change.** Add `ToolCallDelta` and (where available) reasoning events to
`RunEventSink`; surface them in the CLI/dashboard renderers.

**Acceptance criteria.** A test asserts tool-call-argument deltas are emitted during a
streamed tool turn.

**Files.** `crates/apex-agent/src/{runtime.rs,events.rs}`, CLI/dashboard sinks.
**Size.** M. **Depends on:** none.

## RUN-201 `[P2]` — `ai` activity: honor model/params + correct error class

**Problem.** The shared executor's `ai` activity ignores temperature/max_tokens/tools
and always uses the default fast model, and classifies every failure `Retryable`
even for permanent bad-request errors (`crates/apex-runtime/src/lib.rs:197-217`).
(PRD-004 R-A.4; audit Med.)

**Change.** Let an `ai` step pin a model and pass temperature/max_tokens/response_format;
classify permanent (validation/bad-request) errors as non-retryable.

**Acceptance criteria.** A test asserts an `ai` step honors a pinned model and that a
validation error is not retried.

**Files.** `crates/apex-runtime/src/lib.rs`. **Size.** M. **Depends on:** PRV-202.

## RUN-202 `[P2]` — Sub-agent run observability + real cost

**Problem.** `agent` activities run with `NullSink` and record only cost
(`crates/apex-runtime/src/lib.rs:253,257`) — which is $0 for real providers until
PRV-101, making server budget enforcement a no-op. (PRD-004 R-B.1; audit Low.)

**Change.** Attach a real event sink (or a span-emitting sink) to sub-agent runs and
record the PRV-101 cost against the parent project budget.

**Acceptance criteria.** A test asserts a sub-agent activity's cost is non-zero and is
charged to the project's daily accumulator.

**Files.** `crates/apex-runtime/src/lib.rs`. **Size.** S. **Depends on:** PRV-101.

---

# WS-I — Guardrails & Prompt Management

## SAF-201 `[P1]` — Content-safety / moderation / PII hooks

**Problem.** No moderation, PII-redaction, or jailbreak checks anywhere in the agent
loop or provider layer (audit grep). (PRD-004 R-I.1; audit Med.)

**Change.** Add a `Guardrail` trait invoked on model input and output in `run_agent`
(pluggable: a moderation provider, a PII redactor, a jailbreak classifier), fail-closed
or annotate per policy.

**Acceptance criteria.** A test asserts a configured guardrail can block/redact a
flagged input and output; absent config, behavior is unchanged.

**Files.** `crates/apex-agent/src/runtime.rs` + new `guardrail.rs`. **Size.** L.
**Depends on:** none.

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
