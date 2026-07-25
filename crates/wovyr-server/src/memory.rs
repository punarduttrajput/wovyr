//! Memory-explorer routes: namespaces, record browsing, hybrid query, and put.
//!
//! Thin HTTP surface over [`wovyr_memory::MemoryEngine`] (the same engine and on-disk
//! store the CLI's `memory` commands use, under `~/.wovyr/memory`). Retrieval is the
//! engine's hybrid (vector + keyword) search; results carry the ranking
//! `score_breakdown` so the UI can explain why each record matched.
//!
//! `.unwrap()`/`.expect()`/`unreachable!()` on request-derived data are denied here
//! (RM-AIM-P3 SRV-306) — a malformed client request must return a mapped `ApiError`,
//! never panic.

#![cfg_attr(
    not(test),
    warn(clippy::unwrap_used, clippy::expect_used, clippy::unreachable)
)]

use crate::AppState;
use crate::hardening::{PageQuery, paginate};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::HeaderMap,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use utoipa::ToSchema;
use wovyr_memory::{
    AccessContext, EncryptingMemoryStore, FileStore, InMemoryStore, MemoryEngine, MemoryQuery,
    MemoryRecord, MemoryStore, MemoryType, RankingWeights, RetrievalStrategy,
};
use wovyr_provider::Gateway;

/// The required-scope prefix that tags a record with its owning tenant.
const TENANT_SCOPE_PREFIX: &str = "tenant:";

/// The required scope that marks a record as owned by `tenant`.
fn tenant_scope(tenant: &str) -> String {
    format!("{TENANT_SCOPE_PREFIX}{tenant}")
}

/// Whether `tenant` may see a record carrying `required_scopes`. A record tagged with a
/// `tenant:<owner>` scope is visible only to that owner; an untagged record (legacy or
/// CLI-created) belongs to the anonymous `default` space. This is the tenant-isolation
/// filter applied to every read path (the ABAC grants in [`query`] additionally enforce
/// within-tenant scopes).
fn visible_to(required_scopes: &[String], tenant: &str) -> bool {
    match required_scopes
        .iter()
        .find_map(|s| s.strip_prefix(TENANT_SCOPE_PREFIX))
    {
        Some(owner) => owner == tenant,
        None => tenant == crate::tenancy::DEFAULT_TENANT,
    }
}

/// Build the memory engine over the durable `~/.wovyr/memory` file store (shared with the
/// CLI), falling back to in-memory if that directory is unavailable. Always wrapped in
/// [`EncryptingMemoryStore`] over `kms` — transparent and backward-compatible, since it
/// only acts on records with `sensitive: true` (none by default) — so every read path
/// (this engine, plus namespace/record enumeration below) sees plaintext regardless.
/// Returns the engine plus a clone of the (wrapped) store for that enumeration.
/// Build the platform memory engine + store.
///
/// **Fails closed (RM-AR-P1 AIC-301)** when the environment's LLM provider
/// cannot embed (e.g. Anthropic-only, no `OPENAI_API_KEY`): the memory routes
/// are always mounted, so an embedding-less deployment would error on every
/// `remember`/`query`. `MemoryEngine::try_new` surfaces that here, at startup,
/// rather than per-call deep inside a run.
pub(crate) fn default_engine(
    kms: Arc<dyn wovyr_kms::Kms>,
) -> wovyr_common::Result<(MemoryEngine, Arc<dyn MemoryStore>)> {
    let dir = wovyr_config::paths::memory_dir().ok();
    let inner: Arc<dyn MemoryStore> = match dir.and_then(|d| FileStore::new(d).ok()) {
        Some(s) => Arc::new(s),
        None => Arc::new(InMemoryStore::new()),
    };
    let store: Arc<dyn MemoryStore> = Arc::new(EncryptingMemoryStore::new(inner, kms));
    let engine = MemoryEngine::try_new(Gateway::from_env(), store.clone())?;
    Ok((engine, store))
}

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/memory/namespaces", get(list_namespaces))
        .route("/api/v1/memory/records", get(list_records).post(put_record))
        .route("/api/v1/memory:query", post(query))
}

/// Serialize a record for the wire, omitting the (large, non-human) embedding vector.
fn record_json(r: &MemoryRecord) -> Value {
    json!({
        "id": r.id,
        "namespace": r.namespace,
        "content": r.content,
        // `MemoryType` derives its own `snake_case` Serialize (RM-GA-P4 API-702) —
        // this used to re-derive the same string by hand via `{:?}` (Debug) +
        // `to_lowercase()`, which only coincidentally matched serde's output and
        // would have silently diverged for any future multi-word variant.
        "type": r.memory_type,
        "importance": r.importance,
        "tags": r.tags,
        "required_scopes": r.required_scopes,
        "seq": r.seq,
    })
}

fn parse_type(s: Option<&str>) -> MemoryType {
    match s.unwrap_or("semantic").to_ascii_lowercase().as_str() {
        "conversation" => MemoryType::Conversation,
        "workflow" => MemoryType::Workflow,
        "episodic" => MemoryType::Episodic,
        _ => MemoryType::Semantic,
    }
}

fn parse_strategy(s: Option<&str>) -> RetrievalStrategy {
    match s.unwrap_or("hybrid").to_ascii_lowercase().as_str() {
        "vector" => RetrievalStrategy::Vector,
        "keyword" => RetrievalStrategy::Keyword,
        _ => RetrievalStrategy::Hybrid,
    }
}

/// `GET /api/v1/memory/namespaces` — distinct namespaces with their record counts
/// (scoped to the caller's tenant).
#[utoipa::path(
    get,
    path = "/api/v1/memory/namespaces",
    tag = "memory",
    responses((status = 200, description = "The tenant's distinct namespaces with record counts.")),
)]
pub(crate) async fn list_namespaces(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, crate::ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "memory:read")?;
    let records = state.memory_store.all(None).await?;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut total = 0usize;
    for r in records
        .iter()
        .filter(|r| visible_to(&r.required_scopes, &tenant))
    {
        *counts.entry(r.namespace.clone()).or_default() += 1;
        total += 1;
    }
    let namespaces: Vec<Value> = counts
        .into_iter()
        .map(|(namespace, count)| json!({ "namespace": namespace, "count": count }))
        .collect();
    Ok(Json(json!({ "namespaces": namespaces, "total": total })))
}

#[derive(Deserialize)]
pub(crate) struct RecordsQuery {
    namespace: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
}

/// `GET /api/v1/memory/records?namespace=` — browse records (cursor-paginated).
#[utoipa::path(
    get,
    path = "/api/v1/memory/records",
    tag = "memory",
    params(
        ("namespace" = Option<String>, Query, description = "Filter to a namespace."),
        ("limit" = Option<usize>, Query, description = "Max items per page (default 25, max 100)."),
        ("cursor" = Option<String>, Query, description = "Opaque pagination cursor from a prior page's next_cursor."),
    ),
    responses((status = 200, description = "A paginated list of records (newest first).")),
)]
pub(crate) async fn list_records(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<RecordsQuery>,
) -> Result<Json<Value>, crate::ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "memory:read")?;
    let mut records = state.memory_store.all(q.namespace.as_deref()).await?;
    // Tenant isolation: drop records this tenant does not own (the namespace file may
    // hold other tenants' records under the same logical namespace).
    records.retain(|r| visible_to(&r.required_scopes, &tenant));
    // Newest first (by insertion sequence).
    records.sort_by_key(|r| std::cmp::Reverse(r.seq));
    let items: Vec<Value> = records.iter().map(record_json).collect();
    let page = PageQuery {
        limit: q.limit,
        cursor: q.cursor,
    }
    .page();
    Ok(Json(paginate(items, &page)))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct QueryRequest {
    text: String,
    namespace: Option<String>,
    strategy: Option<String>,
    limit: Option<usize>,
    diversity: Option<f32>,
    min_importance: Option<f32>,
    tags: Option<Vec<String>>,
    grants: Option<Vec<String>>,
    relevance: Option<f32>,
    recency: Option<f32>,
    importance: Option<f32>,
}

/// `POST /api/v1/memory:query` — hybrid retrieval with an explainable score breakdown.
/// Returns a **ranked result set, not a page** of a larger collection — there is no
/// cursor/`has_more`, since retrieval is bounded by `limit`/relevance, not an
/// offset into a stable ordering. `data` matches the field name every other list
/// route uses (RM-GA-P4 API-701) even though this route isn't cursor-paginated.
#[utoipa::path(
    post,
    path = "/api/v1/memory:query",
    tag = "memory",
    request_body = QueryRequest,
    responses((status = 200, description = "A ranked result set with an explainable score breakdown (not cursor-paginated).")),
)]
pub(crate) async fn query(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<QueryRequest>,
) -> Result<Json<Value>, crate::ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "memory:read")?;
    let mut q = MemoryQuery::new(req.text);
    q.namespace = req.namespace;
    q.strategy = parse_strategy(req.strategy.as_deref());
    if let Some(l) = req.limit {
        q.limit = l;
    }
    if let Some(d) = req.diversity {
        q.diversity = d.clamp(0.0, 1.0);
    }
    if let Some(m) = req.min_importance {
        q.min_importance = m;
    }
    if let Some(tags) = req.tags {
        q.tags = tags;
    }
    if req.relevance.is_some() || req.recency.is_some() || req.importance.is_some() {
        let d = RankingWeights::default();
        q.weights = RankingWeights {
            relevance: req.relevance.unwrap_or(d.relevance),
            recency: req.recency.unwrap_or(d.recency),
            importance: req.importance.unwrap_or(d.importance),
        };
    }
    // ABAC: grant the caller's own tenant scope (so its tenant-tagged records pass) plus
    // any within-tenant scopes the client supplies — but never let the client assert a
    // `tenant:` grant (that boundary is server-controlled).
    let mut grants: Vec<String> = req
        .grants
        .unwrap_or_default()
        .into_iter()
        .filter(|g| !g.starts_with(TENANT_SCOPE_PREFIX))
        .collect();
    grants.push(tenant_scope(&tenant));
    q.access = Some(AccessContext::new(grants));

    let results = state.memory.query(&q).await?;
    let data: Vec<Value> = results
        .iter()
        // Defense in depth: enforce tenant ownership on the results too (covers
        // untagged/legacy records, which ABAC treats as public).
        .filter(|s| visible_to(&s.record.required_scopes, &tenant))
        .map(|s| {
            json!({
                "id": s.record.id,
                "namespace": s.record.namespace,
                "content": s.record.content,
                // See `record_json`'s comment — `snake_case` via serde, not a
                // hand-rolled Debug/lowercase hack (RM-GA-P4 API-702).
                "type": s.record.memory_type,
                "importance": s.record.importance,
                "tags": s.record.tags,
                "score": s.score,
                "breakdown": {
                    "relevance": s.breakdown.relevance,
                    "recency": s.breakdown.recency,
                    "importance": s.breakdown.importance,
                    "total": s.breakdown.total,
                },
            })
        })
        .collect();
    Ok(Json(json!({ "data": data, "count": data.len() })))
}

#[derive(Deserialize, ToSchema)]
pub(crate) struct PutRequest {
    namespace: String,
    content: String,
    #[serde(rename = "type")]
    memory_type: Option<String>,
    importance: Option<f32>,
    tags: Option<Vec<String>>,
    required_scopes: Option<Vec<String>>,
    /// Seal `content` at rest through the platform KMS
    /// ([Encryption §4](../../docs/13-security/encryption.md#4-application-layer-encryption)).
    /// Defaults to `false`.
    sensitive: Option<bool>,
}

/// `POST /api/v1/memory/records` — store a memory (embedded via the gateway).
#[utoipa::path(
    post,
    path = "/api/v1/memory/records",
    tag = "memory",
    request_body = PutRequest,
    responses((status = 200, description = "Record stored.")),
)]
pub(crate) async fn put_record(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<PutRequest>,
) -> Result<Json<Value>, crate::ApiError> {
    let tenant = crate::tenancy::tenant_authorize(&state, &headers, "memory:write")?;
    // Tag the record with its owning tenant so every read path can scope it. Any
    // client-supplied `tenant:` scope is dropped — the boundary is server-controlled.
    let mut required_scopes: Vec<String> = req
        .required_scopes
        .unwrap_or_default()
        .into_iter()
        .filter(|s| !s.starts_with(TENANT_SCOPE_PREFIX))
        .collect();
    required_scopes.push(tenant_scope(&tenant));
    let id = state
        .memory
        .remember_full(
            req.namespace,
            req.content,
            parse_type(req.memory_type.as_deref()),
            req.importance.unwrap_or(0.5).clamp(0.0, 1.0),
            req.tags.unwrap_or_default(),
            required_scopes,
            req.sensitive.unwrap_or(false),
        )
        .await?;
    Ok(Json(json!({ "id": id, "status": "stored" })))
}
