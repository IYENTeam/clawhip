//! Transactional outbox: inbox commit and downstream intents are atomic.

use serde_json::Value;
use sqlx::types::Json;

use crate::{AppendOutcome, EvidenceLedger, InboxRecord, LedgerError, payload};

/// A downstream intent to persist alongside an inbound event.
#[derive(Debug, Clone)]
pub struct NewOutboxEntry {
    /// Logical destination, e.g. `task-flow` or a sink id.
    pub destination: String,
    /// Opaque payload delivered to the destination.
    pub payload: Value,
}

/// A stored outbox entry awaiting or past dispatch.
#[derive(Debug, Clone)]
pub struct OutboxEntry {
    /// Monotonic id, also the dispatch order.
    pub id: i64,
    /// The inbound event this entry belongs to.
    pub event_id: String,
    /// Logical destination.
    pub destination: String,
    /// Opaque payload.
    pub payload: Value,
}

impl EvidenceLedger {
    /// Atomically commit an inbound event and its outbox entries.
    ///
    /// On the first delivery both the inbox row and every outbox entry are
    /// written in one transaction ([`AppendOutcome::Committed`]). On a
    /// redelivery the inbox conflict rolls the transaction back and no duplicate
    /// outbox rows are written ([`AppendOutcome::Duplicate`]).
    pub async fn accept(
        &self,
        record: &InboxRecord,
        outbox: &[NewOutboxEntry],
    ) -> Result<AppendOutcome, LedgerError> {
        let event_id = record.event_id.trim();
        if event_id.is_empty() {
            return Err(LedgerError::EmptyEventId);
        }
        if outbox
            .iter()
            .any(|entry| entry.destination.trim().is_empty())
        {
            return Err(LedgerError::EmptyDestination);
        }

        let payload_hash = payload::inbox_hash(&record.kind, &record.payload);
        let acceptance_hash = payload::accept_hash(
            &record.kind,
            &record.payload,
            outbox
                .iter()
                .map(|entry| (entry.destination.as_str(), &entry.payload)),
        );
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query(
            "INSERT INTO evidence_inbox \
             (event_id, kind, payload, payload_hash, acceptance_hash) \
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT (event_id) DO NOTHING",
        )
        .bind(event_id)
        .bind(&record.kind)
        .bind(Json(&record.payload))
        .bind(&payload_hash)
        .bind(&acceptance_hash)
        .execute(&mut *tx)
        .await?
        .rows_affected();

        if rows != 1 {
            tx.rollback().await?;
            return self
                .ensure_accept_duplicate_matches(event_id, &payload_hash, &acceptance_hash)
                .await;
        }

        for entry in outbox {
            sqlx::query(
                "INSERT INTO evidence_outbox (event_id, destination, payload) \
                 VALUES ($1, $2, $3)",
            )
            .bind(event_id)
            .bind(entry.destination.trim())
            .bind(Json(&entry.payload))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(AppendOutcome::Committed)
    }

    async fn ensure_accept_duplicate_matches(
        &self,
        event_id: &str,
        expected_payload_hash: &str,
        expected_acceptance_hash: &str,
    ) -> Result<AppendOutcome, LedgerError> {
        let stored: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT payload_hash, acceptance_hash FROM evidence_inbox WHERE event_id = $1",
        )
        .bind(event_id)
        .fetch_one(&self.pool)
        .await?;
        if stored.0.as_deref() == Some(expected_payload_hash)
            && stored.1.as_deref() == Some(expected_acceptance_hash)
        {
            Ok(AppendOutcome::Duplicate)
        } else {
            Err(LedgerError::PayloadMismatch {
                record_type: "transactional accept",
                record_id: event_id.to_string(),
            })
        }
    }

    /// Claim the oldest pending outbox entries for dispatch.
    pub async fn claim_pending_outbox(&self, limit: i64) -> Result<Vec<OutboxEntry>, LedgerError> {
        let rows: Vec<(i64, String, String, Json<Value>)> = sqlx::query_as(
            "SELECT id, event_id, destination, payload FROM evidence_outbox \
             WHERE dispatched_at IS NULL AND dead_lettered_at IS NULL \
             ORDER BY id LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(id, event_id, destination, payload)| OutboxEntry {
                id,
                event_id,
                destination,
                payload: payload.0,
            })
            .collect())
    }

    /// Mark an outbox entry dispatched. Returns `true` only for the transition
    /// from pending to dispatched, so repeated calls are idempotent.
    pub async fn mark_outbox_dispatched(&self, id: i64) -> Result<bool, LedgerError> {
        let rows = sqlx::query(
            "UPDATE evidence_outbox SET dispatched_at = now() \
             WHERE id = $1 AND dispatched_at IS NULL AND dead_lettered_at IS NULL \
             AND lease_token IS NULL",
        )
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(rows == 1)
    }

    /// Count retryable outbox entries still awaiting dispatch.
    pub async fn pending_outbox_count(&self) -> Result<i64, LedgerError> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM evidence_outbox \
             WHERE dispatched_at IS NULL AND dead_lettered_at IS NULL",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    /// Count outbox entries quarantined after repeated dispatch failures.
    pub async fn dead_letter_outbox_count(&self) -> Result<i64, LedgerError> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM evidence_outbox WHERE dead_lettered_at IS NOT NULL",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }
}
