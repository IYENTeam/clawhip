-- op_pi durable evidence inbox (AGI architecture §4.2).
-- A 200 ACK must only follow a durable commit here. event_id is the
-- restart-safe idempotency key: redelivered events collide on the PK and
-- are absorbed by ON CONFLICT DO NOTHING rather than double-processed.
CREATE TABLE IF NOT EXISTS evidence_inbox (
    event_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    payload JSONB NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS evidence_inbox_received_at_idx
ON evidence_inbox (received_at);
