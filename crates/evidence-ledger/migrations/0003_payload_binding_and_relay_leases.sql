-- Bind idempotency keys to immutable content and make the relay safe for
-- concurrent drainers. Existing rows remain nullable and therefore fail closed
-- on duplicate replay until they are migrated by an owner-controlled process.
ALTER TABLE evidence_inbox
    ADD COLUMN IF NOT EXISTS payload_hash TEXT;

ALTER TABLE accepted_receipt_mirror
    ADD COLUMN IF NOT EXISTS payload_hash TEXT;

ALTER TABLE evidence_inbox
    DROP CONSTRAINT IF EXISTS evidence_inbox_payload_hash_shape;
ALTER TABLE evidence_inbox
    ADD CONSTRAINT evidence_inbox_payload_hash_shape
    CHECK (payload_hash IS NULL OR payload_hash ~ '^[0-9a-f]{64}$');

ALTER TABLE accepted_receipt_mirror
    DROP CONSTRAINT IF EXISTS accepted_receipt_mirror_payload_hash_shape;
ALTER TABLE accepted_receipt_mirror
    ADD CONSTRAINT accepted_receipt_mirror_payload_hash_shape
    CHECK (payload_hash IS NULL OR payload_hash ~ '^[0-9a-f]{64}$');

ALTER TABLE evidence_outbox
    ADD COLUMN IF NOT EXISTS attempt_count INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS lease_token TEXT,
    ADD COLUMN IF NOT EXISTS lease_expires_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS dead_lettered_at TIMESTAMPTZ;

ALTER TABLE evidence_outbox
    DROP CONSTRAINT IF EXISTS evidence_outbox_attempt_count_nonnegative;
ALTER TABLE evidence_outbox
    ADD CONSTRAINT evidence_outbox_attempt_count_nonnegative
    CHECK (attempt_count >= 0);

DROP INDEX IF EXISTS evidence_outbox_pending_idx;
CREATE INDEX evidence_outbox_pending_idx ON evidence_outbox (id)
    WHERE dispatched_at IS NULL AND dead_lettered_at IS NULL;
