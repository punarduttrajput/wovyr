//! Tiered durable backends: a Postgres system-of-record + a Qdrant vector index.
//!
//! Implements the storage tiers from
//! [storage-architecture](../../docs/06-memory-engine/storage-architecture.md):
//! [`PostgresStore`] durably holds records and serves keyword (full-text) search;
//! [`QdrantStore`] holds the embeddings and serves vector ANN search; [`TieredStore`]
//! composes them so the engine pushes vector search to Qdrant and keyword search to
//! Postgres, fusing the results ([retrieval](../../docs/06-memory-engine/retrieval.md)).
//!
//! Enabled by the `tiered` cargo feature. Errors map to [`Error::provider`] (an
//! infrastructure failure the gateway/engine treats as transient).

use crate::record::{MemoryRecord, MemoryType};
use crate::store::{MemoryStore, ScoredId};
use apex_common::{Error, Result};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::hash::{Hash, Hasher};

fn pg_err(context: &str, e: impl std::fmt::Display) -> Error {
    Error::provider(format!("postgres {context}: {e}"))
}

fn make_id(namespace: &str, seq: u64) -> String {
    format!("mem-{namespace}-{seq}")
}

fn mt_to_str(mt: MemoryType) -> &'static str {
    match mt {
        MemoryType::Conversation => "conversation",
        MemoryType::Workflow => "workflow",
        MemoryType::Episodic => "episodic",
        MemoryType::Semantic => "semantic",
    }
}

fn mt_from_str(s: &str) -> MemoryType {
    match s {
        "conversation" => MemoryType::Conversation,
        "workflow" => MemoryType::Workflow,
        "episodic" => MemoryType::Episodic,
        _ => MemoryType::Semantic,
    }
}

// ---------------------------------------------------------------------------
// Postgres: durable system of record + keyword (full-text) search
// ---------------------------------------------------------------------------

/// A Postgres-backed record store. Holds the canonical records and serves
/// full-text keyword search via `tsvector`/`ts_rank`.
pub struct PostgresStore {
    client: tokio_postgres::Client,
}

impl PostgresStore {
    /// Connect (NoTls — for a trusted/local DB) and ensure the schema exists.
    pub async fn connect(conn_str: &str) -> Result<Self> {
        let (client, connection) = tokio_postgres::connect(conn_str, tokio_postgres::NoTls)
            .await
            .map_err(|e| pg_err("connect", e))?;
        // Drive the connection in the background for the life of the client.
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::warn!("postgres connection closed: {e}");
            }
        });

        let store = Self { client };
        store.migrate().await?;
        Ok(store)
    }

    async fn migrate(&self) -> Result<()> {
        self.client
            .batch_execute(
                "CREATE SEQUENCE IF NOT EXISTS memory_seq;
                 CREATE TABLE IF NOT EXISTS memory_records (
                     id              TEXT PRIMARY KEY,
                     namespace       TEXT NOT NULL,
                     content         TEXT NOT NULL,
                     embedding       REAL[] NOT NULL,
                     memory_type     TEXT NOT NULL,
                     importance      REAL NOT NULL,
                     tags            TEXT[] NOT NULL,
                     required_scopes TEXT[] NOT NULL DEFAULT '{}',
                     seq             BIGINT NOT NULL
                 );
                 -- Backfill the ABAC column on databases created before it existed.
                 ALTER TABLE memory_records
                     ADD COLUMN IF NOT EXISTS required_scopes TEXT[] NOT NULL DEFAULT '{}';
                 CREATE INDEX IF NOT EXISTS memory_records_ns ON memory_records (namespace);
                 CREATE INDEX IF NOT EXISTS memory_records_fts
                     ON memory_records USING GIN (to_tsvector('english', content));",
            )
            .await
            .map_err(|e| pg_err("migrate", e))?;
        Ok(())
    }

    fn row_to_record(row: &tokio_postgres::Row) -> MemoryRecord {
        MemoryRecord {
            id: row.get("id"),
            namespace: row.get("namespace"),
            content: row.get("content"),
            embedding: row.get("embedding"),
            memory_type: mt_from_str(&row.get::<_, String>("memory_type")),
            importance: row.get("importance"),
            tags: row.get("tags"),
            required_scopes: row.get("required_scopes"),
            seq: row.get::<_, i64>("seq") as u64,
        }
    }
}

#[async_trait]
impl MemoryStore for PostgresStore {
    async fn put(&self, record: MemoryRecord) -> Result<String> {
        let seq: i64 = self
            .client
            .query_one("SELECT nextval('memory_seq')", &[])
            .await
            .map_err(|e| pg_err("nextval", e))?
            .get(0);
        let id = make_id(&record.namespace, seq as u64);
        self.client
            .execute(
                "INSERT INTO memory_records
                   (id, namespace, content, embedding, memory_type, importance, tags, required_scopes, seq)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                &[
                    &id,
                    &record.namespace,
                    &record.content,
                    &record.embedding,
                    &mt_to_str(record.memory_type),
                    &record.importance,
                    &record.tags,
                    &record.required_scopes,
                    &seq,
                ],
            )
            .await
            .map_err(|e| pg_err("insert", e))?;
        Ok(id)
    }

    async fn all(&self, namespace: Option<&str>) -> Result<Vec<MemoryRecord>> {
        let rows = match namespace {
            Some(ns) => {
                self.client
                    .query(
                        "SELECT * FROM memory_records WHERE namespace = $1 ORDER BY seq",
                        &[&ns],
                    )
                    .await
            }
            None => {
                self.client
                    .query("SELECT * FROM memory_records ORDER BY seq", &[])
                    .await
            }
        }
        .map_err(|e| pg_err("select all", e))?;
        Ok(rows.iter().map(Self::row_to_record).collect())
    }

    async fn get(&self, ids: &[String]) -> Result<Vec<MemoryRecord>> {
        let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let rows = self
            .client
            .query(
                "SELECT * FROM memory_records WHERE id = ANY($1)",
                &[&id_refs],
            )
            .await
            .map_err(|e| pg_err("select by id", e))?;
        Ok(rows.iter().map(Self::row_to_record).collect())
    }

    fn supports_pushdown(&self) -> bool {
        true
    }

    async fn keyword_search(
        &self,
        namespace: Option<&str>,
        query: &str,
        k: usize,
    ) -> Result<Option<Vec<ScoredId>>> {
        let limit = k as i64;
        let rows = match namespace {
            Some(ns) => {
                self.client
                    .query(
                        "SELECT id, ts_rank(to_tsvector('english', content),
                                            plainto_tsquery('english', $1)) AS rank
                         FROM memory_records
                         WHERE namespace = $2
                           AND to_tsvector('english', content) @@ plainto_tsquery('english', $1)
                         ORDER BY rank DESC LIMIT $3",
                        &[&query, &ns, &limit],
                    )
                    .await
            }
            None => {
                self.client
                    .query(
                        "SELECT id, ts_rank(to_tsvector('english', content),
                                            plainto_tsquery('english', $1)) AS rank
                         FROM memory_records
                         WHERE to_tsvector('english', content) @@ plainto_tsquery('english', $1)
                         ORDER BY rank DESC LIMIT $2",
                        &[&query, &limit],
                    )
                    .await
            }
        }
        .map_err(|e| pg_err("keyword search", e))?;

        let hits = rows
            .iter()
            .map(|row| (row.get::<_, String>("id"), row.get::<_, f32>("rank")))
            .collect();
        Ok(Some(hits))
    }
}

// ---------------------------------------------------------------------------
// Qdrant: vector ANN index (REST)
// ---------------------------------------------------------------------------

/// A Qdrant-backed vector index addressed over its REST API. Points are keyed by a
/// stable hash of the record id; the record id and namespace live in the payload.
pub struct QdrantStore {
    base_url: String,
    collection: String,
    client: reqwest::Client,
    ready: tokio::sync::Mutex<bool>,
}

/// Deterministic point id from a record id (Qdrant point ids must be numeric/UUID).
fn point_id(record_id: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    record_id.hash(&mut hasher);
    hasher.finish()
}

fn qd_err(context: &str, e: impl std::fmt::Display) -> Error {
    Error::provider(format!("qdrant {context}: {e}"))
}

impl QdrantStore {
    /// Create a client for `collection` at `base_url` (e.g. `http://localhost:6333`).
    pub fn new(base_url: impl Into<String>, collection: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            collection: collection.into(),
            client: reqwest::Client::new(),
            ready: tokio::sync::Mutex::new(false),
        }
    }

    fn collection_url(&self) -> String {
        format!("{}/collections/{}", self.base_url, self.collection)
    }

    /// Create the collection (sized to `dim`, cosine distance) once, if absent.
    async fn ensure_collection(&self, dim: usize) -> Result<()> {
        let mut ready = self.ready.lock().await;
        if *ready {
            return Ok(());
        }
        let exists = self
            .client
            .get(self.collection_url())
            .send()
            .await
            .map_err(|e| qd_err("collection check", e))?
            .status()
            .is_success();
        if !exists {
            let body = json!({ "vectors": { "size": dim, "distance": "Cosine" } });
            let resp = self
                .client
                .put(self.collection_url())
                .json(&body)
                .send()
                .await
                .map_err(|e| qd_err("create collection", e))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(qd_err("create collection", format!("{status}: {text}")));
            }
        }
        *ready = true;
        Ok(())
    }

    /// Upsert one point: the record's embedding plus its id/namespace payload.
    async fn upsert(&self, record_id: &str, namespace: &str, vector: &[f32]) -> Result<()> {
        self.ensure_collection(vector.len()).await?;
        let body = json!({
            "points": [{
                "id": point_id(record_id),
                "vector": vector,
                "payload": { "record_id": record_id, "namespace": namespace }
            }]
        });
        let resp = self
            .client
            .put(format!("{}/points?wait=true", self.collection_url()))
            .json(&body)
            .send()
            .await
            .map_err(|e| qd_err("upsert", e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(qd_err("upsert", format!("{status}: {text}")));
        }
        Ok(())
    }
}

#[async_trait]
impl MemoryStore for QdrantStore {
    async fn put(&self, _record: MemoryRecord) -> Result<String> {
        Err(Error::invalid(
            "QdrantStore is a vector index, not a record store; use TieredStore",
        ))
    }

    async fn all(&self, _namespace: Option<&str>) -> Result<Vec<MemoryRecord>> {
        Err(Error::invalid(
            "QdrantStore does not store records; use TieredStore",
        ))
    }

    fn supports_pushdown(&self) -> bool {
        true
    }

    async fn vector_search(
        &self,
        namespace: Option<&str>,
        query: &[f32],
        k: usize,
    ) -> Result<Option<Vec<ScoredId>>> {
        let mut body = json!({
            "vector": query,
            "limit": k,
            "with_payload": true
        });
        if let Some(ns) = namespace {
            body["filter"] = json!({
                "must": [{ "key": "namespace", "match": { "value": ns } }]
            });
        }

        let resp = self
            .client
            .post(format!("{}/points/search", self.collection_url()))
            .json(&body)
            .send()
            .await
            .map_err(|e| qd_err("search", e))?;

        // A missing collection (nothing indexed yet) means no results, not an error.
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(Some(Vec::new()));
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(qd_err("search", format!("{status}: {text}")));
        }

        let parsed: Value = resp.json().await.map_err(|e| qd_err("decode", e))?;
        let hits = parsed
            .get("result")
            .and_then(Value::as_array)
            .map(|results| {
                results
                    .iter()
                    .filter_map(|hit| {
                        let id = hit.pointer("/payload/record_id").and_then(Value::as_str)?;
                        let score = hit.get("score").and_then(Value::as_f64)? as f32;
                        Some((id.to_string(), score))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok(Some(hits))
    }
}

// ---------------------------------------------------------------------------
// Tiered store: Postgres (records + keyword) + Qdrant (vectors)
// ---------------------------------------------------------------------------

/// Combines a [`PostgresStore`] (system of record + keyword search) with a
/// [`QdrantStore`] (vector ANN). Writes go to both; reads route by capability.
pub struct TieredStore {
    postgres: PostgresStore,
    qdrant: QdrantStore,
}

impl TieredStore {
    /// Connect to Postgres at `pg_conn` and Qdrant at `qdrant_url`, using
    /// `collection` for the vector index.
    pub async fn connect(pg_conn: &str, qdrant_url: &str, collection: &str) -> Result<Self> {
        Ok(Self {
            postgres: PostgresStore::connect(pg_conn).await?,
            qdrant: QdrantStore::new(qdrant_url, collection),
        })
    }
}

#[async_trait]
impl MemoryStore for TieredStore {
    async fn put(&self, record: MemoryRecord) -> Result<String> {
        let namespace = record.namespace.clone();
        let embedding = record.embedding.clone();
        // Postgres is the system of record (assigns id/seq); Qdrant indexes the vector.
        let id = self.postgres.put(record).await?;
        self.qdrant.upsert(&id, &namespace, &embedding).await?;
        Ok(id)
    }

    async fn all(&self, namespace: Option<&str>) -> Result<Vec<MemoryRecord>> {
        self.postgres.all(namespace).await
    }

    async fn get(&self, ids: &[String]) -> Result<Vec<MemoryRecord>> {
        self.postgres.get(ids).await
    }

    fn supports_pushdown(&self) -> bool {
        true
    }

    async fn vector_search(
        &self,
        namespace: Option<&str>,
        query: &[f32],
        k: usize,
    ) -> Result<Option<Vec<ScoredId>>> {
        self.qdrant.vector_search(namespace, query, k).await
    }

    async fn keyword_search(
        &self,
        namespace: Option<&str>,
        query: &str,
        k: usize,
    ) -> Result<Option<Vec<ScoredId>>> {
        self.postgres.keyword_search(namespace, query, k).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_id_is_namespaced_and_deterministic() {
        assert_eq!(make_id("docs", 7), "mem-docs-7");
        assert_eq!(make_id("docs", 7), make_id("docs", 7));
        assert_ne!(make_id("docs", 7), make_id("notes", 7));
        assert_ne!(make_id("docs", 7), make_id("docs", 8));
    }

    #[test]
    fn memory_type_round_trips_through_strings() {
        for mt in [
            MemoryType::Conversation,
            MemoryType::Workflow,
            MemoryType::Episodic,
            MemoryType::Semantic,
        ] {
            assert_eq!(mt_from_str(mt_to_str(mt)), mt);
        }
    }

    #[test]
    fn mt_from_str_defaults_unknown_to_semantic() {
        assert_eq!(mt_from_str("nonsense"), MemoryType::Semantic);
        assert_eq!(mt_from_str(""), MemoryType::Semantic);
    }

    #[test]
    fn point_id_is_stable_and_distinguishes_ids() {
        assert_eq!(point_id("mem-docs-1"), point_id("mem-docs-1"));
        assert_ne!(point_id("mem-docs-1"), point_id("mem-docs-2"));
    }

    #[test]
    fn qdrant_base_url_trims_trailing_slash() {
        let store = QdrantStore::new("http://localhost:6333/", "apex_memory");
        assert_eq!(
            store.collection_url(),
            "http://localhost:6333/collections/apex_memory"
        );
    }

    #[test]
    fn error_helpers_map_to_provider_errors() {
        // Infrastructure failures must be transient (provider), not permanent.
        let pg = pg_err("connect", "refused");
        let qd = qd_err("search", "timeout");
        assert!(pg.to_string().contains("postgres connect"));
        assert!(qd.to_string().contains("qdrant search"));
    }
}
