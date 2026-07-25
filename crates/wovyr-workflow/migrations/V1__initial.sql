-- Initial schema for the Postgres-backed workflow event log + checkpoint
-- store + work queue (RM-GA-P3 MIG-A1). Consolidates what PostgresStore's
-- old inline `migrate()` used to create ad-hoc at every `connect()` call,
-- including the `shard` column/index added after the tables' original
-- introduction — this is a fresh migration history, so V1 already reflects
-- the current end-state schema rather than replaying that history.

CREATE TABLE IF NOT EXISTS workflow_events (
    execution_id TEXT   NOT NULL,
    seq          BIGINT NOT NULL,
    event        TEXT   NOT NULL,
    PRIMARY KEY (execution_id, seq)
);

CREATE TABLE IF NOT EXISTS workflow_checkpoints (
    execution_id TEXT PRIMARY KEY,
    snapshot     TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS workflow_queue (
    execution_id TEXT PRIMARY KEY,
    leased_by    TEXT,
    leased_until TIMESTAMPTZ,
    shard        INT NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS workflow_queue_shard_idx ON workflow_queue (shard);
