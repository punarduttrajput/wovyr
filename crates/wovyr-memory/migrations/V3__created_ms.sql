-- Real creation timestamps (RM-AIM-P2 RAG-205): epoch milliseconds stamped by
-- the engine at ingestion. 0 marks a legacy record written before timestamps
-- existed — recency falls back to sequence distance for those, and they are
-- excluded from time-range-filtered queries.

ALTER TABLE memory_records ADD COLUMN created_ms BIGINT NOT NULL DEFAULT 0;
