-- WFL-104: a dedicated per-execution event-sequence counter.
--
-- The event log previously derived each new seq with `SELECT MAX(seq)+1`, which is
-- safe only under "exactly one appender per execution". A lease-expiry race (an old
-- worker still running while a new one resumes the same execution) produced two
-- concurrent appends that both read the same MAX and collided on the
-- (execution_id, seq) primary key. Allocating the seq via an atomic
-- `UPDATE … SET next_seq = next_seq + 1 RETURNING` on this counter row instead
-- row-locks per execution, so overlapping appenders get distinct, contiguous seqs
-- and never collide. Back-filled from any existing events so an upgraded database
-- continues numbering where it left off.

CREATE TABLE IF NOT EXISTS workflow_event_seq (
    execution_id TEXT   PRIMARY KEY,
    next_seq     BIGINT NOT NULL
);

INSERT INTO workflow_event_seq (execution_id, next_seq)
SELECT execution_id, MAX(seq)
FROM workflow_events
GROUP BY execution_id
ON CONFLICT (execution_id) DO NOTHING;
