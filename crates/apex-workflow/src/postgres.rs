//! Postgres-backed durable store: the event log + checkpoint store on one database.
//!
//! Implements both durability ports ([`EventLog`] + [`CheckpointStore`]) against
//! PostgreSQL so durable `resume` works across processes and nodes, not just one
//! host's filesystem ([persistence §10](../../docs/03-workflow-engine/overview.md)).
//! Events are an append-only table keyed `(execution_id, seq)`; the checkpoint is a
//! single upserted row per execution. Both payloads are stored as JSON text (the
//! same encoding [`FileStore`](crate::FileStore) uses), so no extra Postgres type
//! mapping is needed. Enabled by the `postgres` cargo feature.
//!
//! **Schema migrations (RM-GA-P3 MIG-A1):** `connect` used to run
//! `CREATE TABLE IF NOT EXISTS`/ad-hoc `ALTER TABLE ADD COLUMN IF NOT EXISTS`
//! inline on every call — no version tracking, no down-path, and every process
//! needed DDL privilege just to start serving. Schema changes now live in
//! versioned `migrations/*.sql` files (applied via [`refinery`], tracked in an
//! `apex_workflow_schema_history` table distinct from the other Postgres-backed
//! crates' own history tables so all three can share one physical database
//! without colliding). [`PostgresStore::run_migrations`] is the only thing that
//! ever runs DDL — invoked explicitly via `apex admin migrate`, never by
//! `connect`. `connect` only *reads* the schema version and fails closed
//! (`Error::Config`) if it doesn't match this binary's expected version exactly
//! — too old ("run migrations first") or too new (an old binary must not touch
//! a newer schema it doesn't understand).

use crate::engine::{ExecutionFilter, ExecutionState};
use crate::event::WorkflowEvent;
use crate::queue::{PartitionAssignment, WorkQueue, shard_of};
use crate::store::{CheckpointStore, EventLog};
use apex_common::{Error, Result};
use async_trait::async_trait;
use refinery::AsyncMigrate;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_postgres::Client;

refinery::embed_migrations!("migrations");

/// Distinct per-crate so `apex-workflow`/`apex-memory`/`apex-marketplace` can
/// all migrate the same physical Postgres database without their version
/// tracking colliding.
const MIGRATION_TABLE: &str = "apex_workflow_schema_history";

fn pg_err(context: &str, e: impl std::fmt::Display) -> Error {
    Error::provider(format!("postgres {context}: {e}"))
}

/// This binary's expected schema version — the highest version among its own
/// embedded migrations. Pure/local: no database round-trip needed to know it.
fn expected_schema_version() -> u32 {
    migrations::runner()
        .get_migrations()
        .iter()
        .map(|m| m.version())
        .max()
        .unwrap_or(0)
}

/// Read (never write) the schema version actually applied to `client`, and
/// fail closed if it doesn't match [`expected_schema_version`] exactly.
async fn assert_schema_version(client: &mut tokio_postgres::Client) -> Result<()> {
    let expected = expected_schema_version();
    let applied = AsyncMigrate::get_last_applied_migration(client, MIGRATION_TABLE)
        .await
        .map_err(|e| {
            Error::config(format!(
                "workflow Postgres schema is not migrated (expected version {expected}): {e}; \
                 run `apex admin migrate --target workflow --database-url <url>` first"
            ))
        })?
        .map(|m| m.version())
        .unwrap_or(0);
    if applied < expected {
        return Err(Error::config(format!(
            "workflow Postgres schema is at version {applied}, but this binary needs version \
             {expected}; run `apex admin migrate --target workflow --database-url <url>`"
        )));
    }
    if applied > expected {
        return Err(Error::config(format!(
            "workflow Postgres schema is at version {applied}, newer than this binary's version \
             {expected}; upgrade the apex binary before connecting to this database"
        )));
    }
    Ok(())
}

/// Default max concurrent connections in the pool (WFL-101). Overridable via
/// `APEX_PG_POOL_MAX`.
const DEFAULT_POOL_MAX: usize = 8;

/// Dial one connection (NoTls), spawning its background driver task.
///
/// **WFL-103 (TLS) deferral:** encrypting the connection needs a rustls-0.23-compatible
/// `tokio-postgres-rustls`, but this offline workspace only vendors `tokio-postgres-rustls`
/// 0.10 (which targets rustls 0.21 and fails to compile against this workspace's
/// futures/tokio). So the TLS connector + the refuse-plaintext-to-remote guard are
/// deferred until a compatible crate is vendored; today's `NoTls` behavior is
/// unchanged. Tracked in the WS-H ticket doc.
async fn dial(conn_str: &str) -> Result<Client> {
    let (client, connection) = tokio_postgres::connect(conn_str, tokio_postgres::NoTls)
        .await
        .map_err(|e| pg_err("connect", e))?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::warn!("postgres connection closed: {e}");
        }
    });
    Ok(client)
}

/// A minimal connection pool over `tokio_postgres::Client` (WFL-101): bounds concurrent
/// connections with a semaphore, reuses idle clients, and **transparently reconnects**
/// — a client whose background driver died (`is_closed()`) is discarded on return and a
/// fresh one dialed on the next checkout. Hand-rolled rather than pulling
/// `deadpool`/`bb8`: this workspace builds offline and neither is vendored, and the
/// needs here are modest, matching how apex hand-rolls its S3 signer / cron evaluator.
struct PgPool {
    conn_str: String,
    idle: Mutex<Vec<Client>>,
    permits: Arc<Semaphore>,
    max_size: usize,
}

impl PgPool {
    /// Open the pool, eagerly dialing one connection so a bad URL / unreachable DB
    /// fails fast at `connect()` rather than on the first query.
    async fn open(conn_str: &str, max_size: usize) -> Result<Self> {
        let first = dial(conn_str).await?;
        Ok(Self {
            conn_str: conn_str.to_string(),
            idle: Mutex::new(vec![first]),
            permits: Arc::new(Semaphore::new(max_size)),
            max_size,
        })
    }

    /// Check out a connection (waiting for a slot at capacity), reusing a live idle
    /// client or dialing a fresh one. Concurrent checkouts get *distinct* connections
    /// up to `max_size`, so store calls don't serialize on one socket.
    async fn get(&self) -> Result<PooledConn<'_>> {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| pg_err("pool", e))?;
        // Pop a live idle client (discarding any that closed) without holding the lock
        // across the await below.
        let reused = {
            let mut idle = self.idle.lock().expect("pg pool poisoned");
            loop {
                match idle.pop() {
                    Some(c) if !c.is_closed() => break Some(c),
                    Some(_) => continue,
                    None => break None,
                }
            }
        };
        let client = match reused {
            Some(c) => c,
            None => dial(&self.conn_str).await?,
        };
        Ok(PooledConn {
            pool: self,
            client: Some(client),
            _permit: permit,
        })
    }
}

/// A checked-out connection; returns a live client to the pool on drop, discards a
/// closed one (so the next checkout reconnects).
struct PooledConn<'a> {
    pool: &'a PgPool,
    client: Option<Client>,
    _permit: OwnedSemaphorePermit,
}

impl PooledConn<'_> {
    fn client(&self) -> &Client {
        self.client.as_ref().expect("pooled client present")
    }
}

impl Drop for PooledConn<'_> {
    fn drop(&mut self) {
        if let Some(client) = self.client.take()
            && !client.is_closed()
            && let Ok(mut idle) = self.pool.idle.try_lock()
            && idle.len() < self.pool.max_size
        {
            idle.push(client);
        }
    }
}

/// A PostgreSQL-backed event log + checkpoint store, over a reconnecting pool (WFL-101)
/// with TLS to remote hosts (WFL-103).
pub struct PostgresStore {
    pool: PgPool,
    /// Number of queue partitions; each enqueued execution is assigned a shard
    /// `shard_of(id, partitions)` so worker pools can lease disjoint partitions
    /// without contending (G6). Defaults to 1 (no sharding).
    partitions: u32,
}

impl PostgresStore {
    /// Connect and verify the schema is at the version this binary expects — never runs
    /// DDL (see [`Self::run_migrations`]). Uses a reconnecting pool (WFL-101).
    pub async fn connect(conn_str: &str) -> Result<Self> {
        let max_size = std::env::var("APEX_PG_POOL_MAX")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_POOL_MAX);
        let pool = PgPool::open(conn_str, max_size).await?;

        // Verify the applied schema version on a checked-out connection.
        {
            let mut conn = pool.get().await?;
            let client = conn.client.as_mut().expect("pooled client present");
            assert_schema_version(client).await?;
        }

        Ok(Self {
            pool,
            partitions: 1,
        })
    }

    /// Apply every pending migration, creating the tracking table on first run.
    /// The only place this crate ever issues DDL — called explicitly via
    /// `apex admin migrate`, not from `connect`/`serve`, so the serving path
    /// needs no schema-modification privilege.
    pub async fn run_migrations(conn_str: &str) -> Result<()> {
        let mut client = dial(conn_str).await?;
        migrations::runner()
            .set_migration_table_name(MIGRATION_TABLE)
            .run_async(&mut client)
            .await
            .map_err(|e| pg_err("migrate", e))?;
        Ok(())
    }

    /// Set the number of queue partitions (must be done before enqueuing, and must
    /// match the `total` of every worker pool's [`PartitionAssignment`]).
    pub fn with_partitions(mut self, partitions: u32) -> Self {
        self.partitions = partitions.max(1);
        self
    }
}

#[async_trait]
impl EventLog for PostgresStore {
    async fn append(&self, execution_id: &str, event: WorkflowEvent) -> Result<u64> {
        let payload = serde_json::to_string(&event)?;
        let conn = self.pool.get().await?;
        // Allocate the next per-execution sequence via an atomic upsert on a dedicated
        // counter row (WFL-104): `UPDATE … SET next_seq = next_seq + 1 RETURNING`
        // row-locks the counter, so two overlapping workers on the same execution get
        // *distinct* seqs instead of both computing the same `MAX(seq)+1` and colliding
        // on the `(execution_id, seq)` primary key. The event insert then uses that seq.
        let seq_row = conn
            .client()
            .query_one(
                "INSERT INTO workflow_event_seq (execution_id, next_seq)
                 VALUES ($1, 1)
                 ON CONFLICT (execution_id)
                 DO UPDATE SET next_seq = workflow_event_seq.next_seq + 1
                 RETURNING next_seq",
                &[&execution_id],
            )
            .await
            .map_err(|e| pg_err("allocate event seq", e))?;
        let seq: i64 = seq_row.get(0);
        conn.client()
            .execute(
                "INSERT INTO workflow_events (execution_id, seq, event) VALUES ($1, $2, $3)",
                &[&execution_id, &seq, &payload],
            )
            .await
            .map_err(|e| pg_err("append event", e))?;
        Ok(seq as u64)
    }

    async fn load(&self, execution_id: &str) -> Result<Vec<WorkflowEvent>> {
        let conn = self.pool.get().await?;
        let rows = conn
            .client()
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
        let conn = self.pool.get().await?;
        // Upsert: one latest checkpoint per execution.
        conn.client()
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
        let conn = self.pool.get().await?;
        let row = conn
            .client()
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

    async fn list(&self, filter: &ExecutionFilter) -> Result<Vec<ExecutionState>> {
        // Ordered by execution id at the database; name/status/limit are applied in
        // Rust over the decoded snapshots (a dedicated index column is a later slice).
        let conn = self.pool.get().await?;
        let rows = conn
            .client()
            .query(
                "SELECT snapshot FROM workflow_checkpoints ORDER BY execution_id",
                &[],
            )
            .await
            .map_err(|e| pg_err("list checkpoints", e))?;
        let mut snapshots = Vec::with_capacity(rows.len());
        for row in &rows {
            let state: ExecutionState = serde_json::from_str(row.get::<_, &str>("snapshot"))?;
            if filter.matches(&state) {
                snapshots.push(state);
            }
        }
        if let Some(limit) = filter.limit {
            snapshots.truncate(limit);
        }
        Ok(snapshots)
    }
}

#[async_trait]
impl WorkQueue for PostgresStore {
    async fn enqueue(&self, execution_id: &str) -> Result<()> {
        let conn = self.pool.get().await?;
        conn.client()
            .execute(
                "INSERT INTO workflow_queue (execution_id, shard) VALUES ($1, $2)
                 ON CONFLICT (execution_id) DO NOTHING",
                &[
                    &execution_id,
                    &(shard_of(execution_id, self.partitions) as i32),
                ],
            )
            .await
            .map_err(|e| pg_err("enqueue", e))?;
        Ok(())
    }

    async fn lease(&self, worker: &str, ttl: Duration) -> Result<Option<String>> {
        // Atomically claim one ready row; `SKIP LOCKED` lets concurrent workers take
        // disjoint executions without blocking each other.
        let secs = ttl.as_secs_f64();
        let conn = self.pool.get().await?;
        let row = conn
            .client()
            .query_opt(
                "WITH picked AS (
                     SELECT execution_id FROM workflow_queue
                     WHERE leased_by IS NULL OR leased_until < now()
                     ORDER BY execution_id
                     FOR UPDATE SKIP LOCKED
                     LIMIT 1
                 )
                 UPDATE workflow_queue q
                 SET leased_by = $1, leased_until = now() + make_interval(secs => $2)
                 FROM picked WHERE q.execution_id = picked.execution_id
                 RETURNING q.execution_id",
                &[&worker, &secs],
            )
            .await
            .map_err(|e| pg_err("lease", e))?;
        Ok(row.map(|r| r.get::<_, String>("execution_id")))
    }

    async fn lease_sharded(
        &self,
        worker: &str,
        assignment: &PartitionAssignment,
        ttl: Duration,
    ) -> Result<Option<String>> {
        // Same atomic claim as `lease`, scoped to the partitions this pool owns, so
        // disjoint pools never lock the same rows (G6).
        let secs = ttl.as_secs_f64();
        let owned: Vec<i32> = assignment.owned.iter().map(|s| *s as i32).collect();
        let conn = self.pool.get().await?;
        let row = conn
            .client()
            .query_opt(
                "WITH picked AS (
                     SELECT execution_id FROM workflow_queue
                     WHERE (leased_by IS NULL OR leased_until < now()) AND shard = ANY($3)
                     ORDER BY execution_id
                     FOR UPDATE SKIP LOCKED
                     LIMIT 1
                 )
                 UPDATE workflow_queue q
                 SET leased_by = $1, leased_until = now() + make_interval(secs => $2)
                 FROM picked WHERE q.execution_id = picked.execution_id
                 RETURNING q.execution_id",
                &[&worker, &secs, &owned],
            )
            .await
            .map_err(|e| pg_err("lease_sharded", e))?;
        Ok(row.map(|r| r.get::<_, String>("execution_id")))
    }

    async fn renew(&self, execution_id: &str, worker: &str, ttl: Duration) -> Result<()> {
        let secs = ttl.as_secs_f64();
        let conn = self.pool.get().await?;
        conn.client()
            .execute(
                "UPDATE workflow_queue SET leased_until = now() + make_interval(secs => $3)
                 WHERE execution_id = $1 AND leased_by = $2",
                &[&execution_id, &worker, &secs],
            )
            .await
            .map_err(|e| pg_err("renew lease", e))?;
        Ok(())
    }

    async fn remove(&self, execution_id: &str) -> Result<()> {
        let conn = self.pool.get().await?;
        conn.client()
            .execute(
                "DELETE FROM workflow_queue WHERE execution_id = $1",
                &[&execution_id],
            )
            .await
            .map_err(|e| pg_err("remove from queue", e))?;
        Ok(())
    }
}
