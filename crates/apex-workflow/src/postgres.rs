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

/// How the connection to Postgres is secured (WFL-103).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TlsMode {
    /// Plaintext — only permitted to a loopback / Unix-socket host.
    Disabled,
    /// TLS via rustls (`ring` provider).
    Rustls,
}

/// Decide the TLS mode for `conn_str`, **refusing plaintext to a non-loopback host**
/// (WFL-103). Pure (no I/O), so the refusal is unit-testable without a database:
///
/// - TLS is used when the URL requests it (`sslmode=require`) or `APEX_PG_TLS=1` is set.
/// - A non-loopback host with no TLS signal is refused (`Error::Config`) rather than
///   silently sending credentials + data in the clear.
/// - A loopback / Unix-socket host may use plaintext (the trusted-local default).
fn resolve_tls_mode(conn_str: &str) -> Result<TlsMode> {
    let config: tokio_postgres::Config = conn_str
        .parse()
        .map_err(|e| Error::config(format!("invalid Postgres connection string: {e}")))?;

    // tokio-postgres surfaces only Disable/Prefer/Require. `Require` (and the explicit
    // `APEX_PG_TLS=1`) count as "encrypt"; `Prefer` (the libpq default) is treated as
    // no explicit intent so it doesn't silently satisfy the remote-host requirement.
    let wants_tls = matches!(
        config.get_ssl_mode(),
        tokio_postgres::config::SslMode::Require
    ) || std::env::var("APEX_PG_TLS")
        .map(|v| v == "1")
        .unwrap_or(false);

    let all_loopback = config.get_hosts().iter().all(is_loopback_host);
    if !all_loopback && !wants_tls {
        return Err(Error::config(
            "refusing a plaintext Postgres connection to a non-loopback host; use \
             `sslmode=require` (or set APEX_PG_TLS=1) so credentials and data are \
             encrypted in transit",
        ));
    }
    Ok(if wants_tls {
        TlsMode::Rustls
    } else {
        TlsMode::Disabled
    })
}

/// Whether a parsed host is loopback / a local Unix socket (plaintext-safe).
fn is_loopback_host(host: &tokio_postgres::config::Host) -> bool {
    match host {
        tokio_postgres::config::Host::Tcp(h) => {
            h == "localhost"
                || h.parse::<std::net::IpAddr>()
                    .map(|ip| ip.is_loopback())
                    .unwrap_or(false)
        }
        // A Unix-domain socket (unix platforms only) never leaves the machine.
        #[cfg(unix)]
        tokio_postgres::config::Host::Unix(_) => true,
    }
}

/// A `ServerCertVerifier` that accepts any certificate — the rustls equivalent of
/// libpq `sslmode=require` (encrypt, don't verify), which is what lets a managed DB
/// with a private project CA (e.g. Aiven) connect without its CA bundle. Signature
/// checks still run (delegated to the ring provider's algorithms), so this only skips
/// *identity* verification, exactly as `require` specifies. Opt into full root
/// verification with `APEX_PG_TLS_VERIFY=1`.
#[derive(Debug)]
struct AcceptAnyServerCert {
    algs: rustls::crypto::WebPkiSupportedAlgorithms,
}

impl AcceptAnyServerCert {
    fn new() -> Self {
        Self {
            algs: rustls::crypto::ring::default_provider().signature_verification_algorithms,
        }
    }
}

impl rustls::client::danger::ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.algs.supported_schemes()
    }
}

/// Build the rustls connector: `ring` provider, with the provider passed explicitly so
/// this crate needs no process-global default. Verifies against the Mozilla webpki
/// roots when `APEX_PG_TLS_VERIFY=1` (for a public-CA host); otherwise encrypts without
/// identity verification (libpq `require` semantics — see [`AcceptAnyServerCert`]).
fn rustls_connector() -> Result<tokio_postgres_rustls::MakeRustlsConnect> {
    let builder = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| pg_err("tls config", e))?;

    let config = if std::env::var("APEX_PG_TLS_VERIFY")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        builder.with_root_certificates(roots).with_no_client_auth()
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(std::sync::Arc::new(AcceptAnyServerCert::new()))
            .with_no_client_auth()
    };
    Ok(tokio_postgres_rustls::MakeRustlsConnect::new(config))
}

/// Dial one connection under `tls`, spawning its background driver task. The two TLS
/// types (`NoTls` vs `MakeRustlsConnect`) differ but both yield a uniform `Client`, so
/// the pool stores clients without caring which transport backs them.
async fn dial(conn_str: &str, tls: TlsMode) -> Result<Client> {
    match tls {
        TlsMode::Disabled => {
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
        TlsMode::Rustls => {
            let (client, connection) = tokio_postgres::connect(conn_str, rustls_connector()?)
                .await
                .map_err(|e| pg_err("connect", e))?;
            tokio::spawn(async move {
                if let Err(e) = connection.await {
                    tracing::warn!("postgres connection closed: {e}");
                }
            });
            Ok(client)
        }
    }
}

/// A minimal connection pool over `tokio_postgres::Client` (WFL-101): bounds concurrent
/// connections with a semaphore, reuses idle clients, and **transparently reconnects**
/// — a client whose background driver died (`is_closed()`) is discarded on return and a
/// fresh one dialed on the next checkout. Hand-rolled rather than pulling
/// `deadpool`/`bb8`: this workspace builds offline and neither is vendored, and the
/// needs here are modest, matching how apex hand-rolls its S3 signer / cron evaluator.
struct PgPool {
    conn_str: String,
    tls: TlsMode,
    idle: Mutex<Vec<Client>>,
    permits: Arc<Semaphore>,
    max_size: usize,
}

impl PgPool {
    /// Open the pool, eagerly dialing one connection so a bad URL / unreachable DB
    /// fails fast at `connect()` rather than on the first query.
    async fn open(conn_str: &str, tls: TlsMode, max_size: usize) -> Result<Self> {
        let first = dial(conn_str, tls).await?;
        Ok(Self {
            conn_str: conn_str.to_string(),
            tls,
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
            None => dial(&self.conn_str, self.tls).await?,
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
    /// DDL (see [`Self::run_migrations`]). Uses a reconnecting pool (WFL-101) and TLS
    /// for non-loopback hosts (WFL-103, [`resolve_tls_mode`]).
    pub async fn connect(conn_str: &str) -> Result<Self> {
        let tls = resolve_tls_mode(conn_str)?;
        let max_size = std::env::var("APEX_PG_POOL_MAX")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_POOL_MAX);
        let pool = PgPool::open(conn_str, tls, max_size).await?;

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
    /// needs no schema-modification privilege. Honors WFL-103 TLS selection.
    pub async fn run_migrations(conn_str: &str) -> Result<()> {
        let tls = resolve_tls_mode(conn_str)?;
        let mut client = dial(conn_str, tls).await?;
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
        let payload = crate::event::encode_event(&event)?;
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
            .map(|row| crate::event::decode_event(row.get::<_, &str>("event")))
            .collect()
    }

    async fn load_page(
        &self,
        execution_id: &str,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<WorkflowEvent>> {
        // Bounded at the database (WFL-304): unlike `load`, this never pulls rows
        // beyond the requested page across the wire at all.
        let conn = self.pool.get().await?;
        let rows = conn
            .client()
            .query(
                "SELECT event FROM workflow_events WHERE execution_id = $1
                 ORDER BY seq OFFSET $2 LIMIT $3",
                &[&execution_id, &(offset as i64), &(limit as i64)],
            )
            .await
            .map_err(|e| pg_err("load event page", e))?;
        rows.iter()
            .map(|row| crate::event::decode_event(row.get::<_, &str>("event")))
            .collect()
    }

    async fn compact(&self, execution_id: &str, keep_after_seq: u64) -> Result<()> {
        let conn = self.pool.get().await?;
        conn.client()
            .execute(
                "DELETE FROM workflow_events WHERE execution_id = $1 AND seq <= $2",
                &[&execution_id, &(keep_after_seq as i64)],
            )
            .await
            .map_err(|e| pg_err("compact events", e))?;
        Ok(())
    }
}

#[async_trait]
impl CheckpointStore for PostgresStore {
    async fn save(&self, snapshot: &ExecutionState) -> Result<()> {
        let payload = serde_json::to_string(snapshot)?;
        // WFL-305: `workflow_name`/`status` ride along as their own indexed
        // columns, kept in lockstep with the JSON snapshot on every upsert, so
        // `list()` can filter in SQL instead of decoding every row in Rust.
        let status = status_str(snapshot.status);
        let conn = self.pool.get().await?;
        conn.client()
            .execute(
                "INSERT INTO workflow_checkpoints (execution_id, snapshot, workflow_name, status)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (execution_id) DO UPDATE SET
                     snapshot = EXCLUDED.snapshot,
                     workflow_name = EXCLUDED.workflow_name,
                     status = EXCLUDED.status",
                &[
                    &snapshot.execution_id,
                    &payload,
                    &snapshot.workflow_name,
                    &status,
                ],
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
        // WFL-305: name/status/limit are pushed into the query itself, against the
        // indexed `workflow_name`/`status` columns — a filtered call never even
        // reads, let alone JSON-decodes, a row that doesn't match.
        let name = filter.workflow_name.clone();
        let status = filter.status.map(status_str);
        let limit = filter.limit.map(|l| l as i64);

        let mut sql = String::from("SELECT snapshot FROM workflow_checkpoints WHERE true");
        let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();
        if let Some(name) = &name {
            params.push(name);
            sql.push_str(&format!(" AND workflow_name = ${}", params.len()));
        }
        if let Some(status) = &status {
            params.push(status);
            sql.push_str(&format!(" AND status = ${}", params.len()));
        }
        sql.push_str(" ORDER BY execution_id");
        if let Some(limit) = &limit {
            params.push(limit);
            sql.push_str(&format!(" LIMIT ${}", params.len()));
        }

        let conn = self.pool.get().await?;
        let rows = conn
            .client()
            .query(&sql, &params)
            .await
            .map_err(|e| pg_err("list checkpoints", e))?;
        rows.iter()
            .map(|row| serde_json::from_str(row.get::<_, &str>("snapshot")).map_err(Error::from))
            .collect()
    }
}

/// The exact `snake_case` wire string a [`WorkflowState`] serializes to (`running`,
/// `completed`, …) — derived from the real `Serialize` impl rather than
/// hand-duplicated, so the indexed `status` column can never drift from what the
/// JSON snapshot itself would encode.
fn status_str(status: crate::state::WorkflowState) -> String {
    match serde_json::to_value(status) {
        Ok(serde_json::Value::String(s)) => s,
        other => unreachable!("WorkflowState must serialize to a string, got {other:?}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// WFL-103: the refuse-plaintext-to-non-loopback guard is pure, so it's unit-tested
    /// without a database. (Assumes `APEX_PG_TLS` is unset, as it is in CI/local test
    /// runs — the guard only refuses when there's no TLS signal at all.)
    #[test]
    fn tls_guard_refuses_plaintext_to_remote_but_allows_loopback() {
        // Loopback + no TLS → plaintext allowed.
        assert_eq!(
            resolve_tls_mode("postgres://u:p@127.0.0.1:5432/db").unwrap(),
            TlsMode::Disabled
        );
        assert_eq!(
            resolve_tls_mode("postgres://u:p@localhost:5432/db").unwrap(),
            TlsMode::Disabled
        );
        // Remote + no TLS signal → refused (fail closed).
        assert!(
            resolve_tls_mode("postgres://u:p@db.example.com:5432/db").is_err(),
            "a non-loopback host without TLS must be refused"
        );
        // Remote + sslmode=require → TLS.
        assert_eq!(
            resolve_tls_mode("postgres://u:p@db.example.com:5432/db?sslmode=require").unwrap(),
            TlsMode::Rustls
        );
    }
}
