-- WFL-305: promote workflow_name/status to real, indexed columns on
-- workflow_checkpoints, instead of `list()` scanning and JSON-decoding every
-- row and filtering in Rust. Backfilled from the existing `snapshot` JSON text
-- for any pre-existing rows (a fresh install has none to backfill).

ALTER TABLE workflow_checkpoints ADD COLUMN IF NOT EXISTS workflow_name TEXT;
ALTER TABLE workflow_checkpoints ADD COLUMN IF NOT EXISTS status TEXT;

UPDATE workflow_checkpoints
SET workflow_name = COALESCE(workflow_name, snapshot::jsonb ->> 'workflow_name'),
    status = COALESCE(status, snapshot::jsonb ->> 'status')
WHERE workflow_name IS NULL OR status IS NULL;

ALTER TABLE workflow_checkpoints ALTER COLUMN workflow_name SET NOT NULL;
ALTER TABLE workflow_checkpoints ALTER COLUMN status SET NOT NULL;

CREATE INDEX IF NOT EXISTS workflow_checkpoints_workflow_name_idx
    ON workflow_checkpoints (workflow_name);
CREATE INDEX IF NOT EXISTS workflow_checkpoints_status_idx
    ON workflow_checkpoints (status);
