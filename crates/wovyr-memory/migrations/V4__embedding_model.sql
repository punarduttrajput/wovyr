-- RM-AIM-P3 RAG-301: record which embedding model produced each vector, so
-- re-embedding migration can detect stale records instead of trusting a mixed
-- store. '' marks a legacy record (or a non-embedded parent) — migration
-- treats "unknown" as stale rather than assuming it matches the current model.

ALTER TABLE memory_records ADD COLUMN embedding_model TEXT NOT NULL DEFAULT '';
