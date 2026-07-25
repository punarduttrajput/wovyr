-- Initial schema for the Postgres-backed memory record store (RM-GA-P3
-- MIG-A1). Consolidates what PostgresStore's old inline `migrate()` used to
-- create ad-hoc at every `connect()` call, including the `required_scopes`/
-- `sensitive` columns added after the table's original introduction — this
-- is a fresh migration history, so V1 already reflects the current
-- end-state schema rather than replaying that history.

CREATE SEQUENCE IF NOT EXISTS memory_seq;

CREATE TABLE IF NOT EXISTS memory_records (
    id              TEXT PRIMARY KEY,
    namespace       TEXT NOT NULL,
    content         TEXT NOT NULL,
    embedding       REAL[] NOT NULL,
    memory_type     TEXT NOT NULL,
    importance      REAL NOT NULL,
    tags            TEXT[] NOT NULL,
    required_scopes TEXT[] NOT NULL DEFAULT '{}',
    sensitive       BOOLEAN NOT NULL DEFAULT FALSE,
    seq             BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS memory_records_ns ON memory_records (namespace);
CREATE INDEX IF NOT EXISTS memory_records_fts
    ON memory_records USING GIN (to_tsvector('english', content));
