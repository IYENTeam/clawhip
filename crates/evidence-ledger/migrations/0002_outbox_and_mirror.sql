-- Transactional outbox + accepted-receipt mirror (AGI architecture §4.2, roadmap M2-W3/W4).
--
-- evidence_outbox: written in the SAME transaction as the inbox commit, so an
-- event and its downstream intents are all-or-nothing. A relay drains pending
-- rows and stamps dispatched_at; redelivery is safe because downstream dedupes.
--
-- accepted_receipt_mirror: append-only log of receipts Task Flow already
-- accepted. op_pi mirrors decisions made elsewhere and originates none
-- (ADR-011: append-only-accepted, no-authority-origination).
CREATE TABLE IF NOT EXISTS evidence_outbox (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    event_id      TEXT NOT NULL REFERENCES evidence_inbox (event_id),
    destination   TEXT NOT NULL,
    payload       JSONB NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    dispatched_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS evidence_outbox_pending_idx
    ON evidence_outbox (id) WHERE dispatched_at IS NULL;

CREATE TABLE IF NOT EXISTS accepted_receipt_mirror (
    receipt_id  TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL,
    kind        TEXT NOT NULL,
    body        JSONB NOT NULL,
    mirrored_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
