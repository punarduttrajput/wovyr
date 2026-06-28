//! Postgres-backed durable store: the event log + checkpoint store on one database.
//!
//! Implements both durability ports ([`EventLog`] + [`CheckpointStore`]) against
//! PostgreSQL so durable `resume` works across processes and nodes, not just one
//! host's filesystem ([persistence §10](../../docs/03-workflow-engine/overview.md)).
//! Events are an append-only table keyed `(execution_id, seq)`; the checkpoint is a
//! single upserted row per execution. Both payloads are stored as JSON text (the
//! same encoding [`FileStore`](crate::FileStore) uses), so no extra Postgres type
//! mapping is needed. Enabled by the `postgres` cargo feature.

use crate::engine::ExecutionState;
use crate::event::WorkflowEvent;
use crate::store::{CheckpointStore, EventLog};
use apex_common::{Error, Result};
use async_trait::async_trait;

fn pg_err(context: &str, e: impl std::fmt::Display) -> Error {
    Error::provider(format!("postgres {context}: {e}"))
}

/// A PostgreSQL-backed event log + checkpoint store.
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
                "CREATE TABLE IF NOT EXISTS workflow_events (
                     execution_id TEXT   NOT NULL,
                     seq          BIGINT NOT NULL,
                     event        TEXT   NOT NULL,
                     PRIMARY KEY (execution_id, seq)
                 );
                 CREATE TABLE IF NOT EXISTS workflow_checkpoints (
                     execution_id TEXT PRIMARY KEY,
                     snapshot     TEXT NOT NULL
                 );",
            )
            .await
            .map_err(|e| pg_err("migrate", e))?;
        Ok(())
    }
}

#[async_trait]
impl EventLog for PostgresStore {
    async fn append(&self, execution_id: &str, event: WorkflowEvent) -> Result<u64> {
        let payload = serde_json::to_string(&event)?;
        // Assign the next contiguous 1-based sequence atomically within the insert.
        // Appends for a single execution are serial (the engine drives one step at a
        // time), so the (execution_id, seq) primary key never collides.
        let row = self
            .client
            .query_one(
                "INSERT INTO workflow_events (execution_id, seq, event)
                 VALUES (
                     $1,
                     (SELECT COALESCE(MAX(seq), 0) + 1 FROM workflow_events WHERE execution_id = $1),
                     $2
                 )
                 RETURNING seq",
                &[&execution_id, &payload],
            )
            .await
            .map_err(|e| pg_err("append event", e))?;
        Ok(row.get::<_, i64>(0) as u64)
    }

    async fn load(&self, execution_id: &str) -> Result<Vec<WorkflowEvent>> {
        let rows = self
            .client
            .query(
                "SELECT event FROM workflow_events WHERE execution_id = $1 ORDER BY seq",
                &[&execution_id],
            )
            .await
            .map_err(|e| pg_err("load events", e))?;
        rows.iter()
            .map(|row| serde_json::from_str(row.get::<_, &str>("event")).map_err(Error::from))
            .collect()
    }
}

#[async_trait]
impl CheckpointStore for PostgresStore {
    async fn save(&self, snapshot: &ExecutionState) -> Result<()> {
        let payload = serde_json::to_string(snapshot)?;
        // Upsert: one latest checkpoint per execution.
        self.client
            .execute(
                "INSERT INTO workflow_checkpoints (execution_id, snapshot)
                 VALUES ($1, $2)
                 ON CONFLICT (execution_id) DO UPDATE SET snapshot = EXCLUDED.snapshot",
                &[&snapshot.execution_id, &payload],
            )
            .await
            .map_err(|e| pg_err("save checkpoint", e))?;
        Ok(())
    }

    async fn latest(&self, execution_id: &str) -> Result<Option<ExecutionState>> {
        let row = self
            .client
            .query_opt(
                "SELECT snapshot FROM workflow_checkpoints WHERE execution_id = $1",
                &[&execution_id],
            )
            .await
            .map_err(|e| pg_err("load checkpoint", e))?;
        match row {
            Some(row) => Ok(Some(serde_json::from_str(row.get::<_, &str>("snapshot"))?)),
            None => Ok(None),
        }
    }
}
