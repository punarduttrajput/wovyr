-- Parent-document linkage for chunked ingestion (RM-AIM-P2 RAG-201):
-- `parent_id` points a chunk record at the record holding its full source
-- document; `is_parent` marks that full-document record, which is excluded
-- from retrieval (expansion-only) and from the Qdrant vector index.

ALTER TABLE memory_records ADD COLUMN parent_id TEXT NULL;
ALTER TABLE memory_records ADD COLUMN is_parent BOOLEAN NOT NULL DEFAULT FALSE;

CREATE INDEX memory_records_parent ON memory_records (parent_id)
    WHERE parent_id IS NOT NULL;
