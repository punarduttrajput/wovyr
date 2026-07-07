-- Initial schema for the Postgres-backed marketplace registry (RM-GA-P3
-- MIG-A1). Consolidates what PostgresRegistryStore's old inline
-- `connect()`-time DDL used to create ad-hoc on every connection.

CREATE TABLE IF NOT EXISTS marketplace_listings (
    id      TEXT PRIMARY KEY,
    listing TEXT NOT NULL
);
