-- Bind a transactional accept replay to its ordered outbox intents as well as
-- its inbox payload. Legacy rows remain NULL and fail closed on replay.
ALTER TABLE evidence_inbox
    ADD COLUMN IF NOT EXISTS acceptance_hash TEXT;

ALTER TABLE evidence_inbox
    DROP CONSTRAINT IF EXISTS evidence_inbox_acceptance_hash_shape;
ALTER TABLE evidence_inbox
    ADD CONSTRAINT evidence_inbox_acceptance_hash_shape
    CHECK (acceptance_hash IS NULL OR acceptance_hash ~ '^[0-9a-f]{64}$');
