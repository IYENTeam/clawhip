//! Transactional outbox: inbox commit and downstream intents are atomic.

use serde_json::Value;
use sqlx::types::Json;

use crate::{AppendOutcome, EvidenceLedger, InboxRecord, LedgerError};

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

        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query(
            "INSERT INTO evidence_inbox (event_id, kind, payload) \
             VALUES ($1, $2, $3) ON CONFLICT (event_id) DO NOTHING",
        )
        .bind(event_id)
        .bind(&record.kind)
        .bind(Json(&record.payload))
        .execute(&mut *tx)
        .await?
        .rows_affected();

        if rows != 1 {
            tx.rollback().await?;
            return Ok(AppendOutcome::Duplicate);
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

    /// Claim the oldest pending outbox entries for dispatch.
    pub async fn claim_pending_outbox(&self, limit: i64) -> Result<Vec<OutboxEntry>, LedgerError> {
        let rows: Vec<(i64, String, String, Json<Value>)> = sqlx::query_as(
            "SELECT id, event_id, destination, payload FROM evidence_outbox \
             WHERE dispatched_at IS NULL ORDER BY id LIMIT $1",
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
             WHERE id = $1 AND dispatched_at IS NULL",
        )
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(rows == 1)
    }

    /// Count outbox entries still awaiting dispatch.
    pub async fn pending_outbox_count(&self) -> Result<i64, LedgerError> {
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM evidence_outbox WHERE dispatched_at IS NULL")
                .fetch_one(&self.pool)
                .await?;
        Ok(count)
    }
}
