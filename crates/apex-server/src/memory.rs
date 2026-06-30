//! Memory-explorer routes: namespaces, record browsing, hybrid query, and put.
//!
//! Thin HTTP surface over [`apex_memory::MemoryEngine`] (the same engine and on-disk
//! store the CLI's `memory` commands use, under `~/.apex/memory`). Retrieval is the
//! engine's hybrid (vector + keyword) search; results carry the ranking
//! `score_breakdown` so the UI can explain why each record matched.

use crate::AppState;
use crate::hardening::{PageQuery, paginate};
use apex_memory::{
    AccessContext, FileStore, InMemoryStore, MemoryEngine, MemoryQuery, MemoryRecord, MemoryStore,
    MemoryType, RankingWeights, RetrievalStrategy,
};
use apex_provider::Gateway;
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Build the memory engine over the durable `~/.apex/memory` file store (shared with the
/// CLI), falling back to in-memory if that directory is unavailable. Returns the engine
/// plus a clone of the store for namespace/record enumeration.
pub(crate) fn default_engine() -> (MemoryEngine, Arc<dyn MemoryStore>) {
    let dir = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| std::path::PathBuf::from(home).join(".apex").join("memory"));
    let store: Arc<dyn MemoryStore> = match dir.and_then(|d| FileStore::new(d).ok()) {
        Some(s) => Arc::new(s),
        None => Arc::new(InMemoryStore::new()),
    };
    let engine = MemoryEngine::new(Gateway::from_env(), store.clone());
    (engine, store)
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
        "type": format!("{:?}", r.memory_type).to_lowercase(),
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

/// `GET /api/v1/memory/namespaces` — distinct namespaces with their record counts.
async fn list_namespaces(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, crate::ApiError> {
    let records = state.memory_store.all(None).await?;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut total = 0usize;
    for r in &records {
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
struct RecordsQuery {
    namespace: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
}

/// `GET /api/v1/memory/records?namespace=` — browse records (cursor-paginated).
async fn list_records(
    State(state): State<Arc<AppState>>,
    Query(q): Query<RecordsQuery>,
) -> Result<Json<Value>, crate::ApiError> {
    let mut records = state.memory_store.all(q.namespace.as_deref()).await?;
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

#[derive(Deserialize)]
struct QueryRequest {
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
async fn query(
    State(state): State<Arc<AppState>>,
    Json(req): Json<QueryRequest>,
) -> Result<Json<Value>, crate::ApiError> {
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
    if let Some(grants) = req.grants {
        q.access = Some(AccessContext::new(grants));
    }

    let results = state.memory.query(&q).await?;
    let data: Vec<Value> = results
        .iter()
        .map(|s| {
            json!({
                "id": s.record.id,
                "namespace": s.record.namespace,
                "content": s.record.content,
                "type": format!("{:?}", s.record.memory_type).to_lowercase(),
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
    Ok(Json(json!({ "results": data, "count": data.len() })))
}

#[derive(Deserialize)]
struct PutRequest {
    namespace: String,
    content: String,
    #[serde(rename = "type")]
    memory_type: Option<String>,
    importance: Option<f32>,
    tags: Option<Vec<String>>,
    required_scopes: Option<Vec<String>>,
}

/// `POST /api/v1/memory/records` — store a memory (embedded via the gateway).
async fn put_record(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PutRequest>,
) -> Result<Json<Value>, crate::ApiError> {
    let id = state
        .memory
        .remember_scoped(
            req.namespace,
            req.content,
            parse_type(req.memory_type.as_deref()),
            req.importance.unwrap_or(0.5).clamp(0.0, 1.0),
            req.tags.unwrap_or_default(),
            req.required_scopes.unwrap_or_default(),
        )
        .await?;
    Ok(Json(json!({ "id": id, "status": "stored" })))
}
