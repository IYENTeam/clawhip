//! Durable inbox: commit-before-ack with restart-safe idempotent dedupe.

use serde_json::Value;
use sqlx::types::Json;

use crate::{EvidenceLedger, LedgerError};

/// Result of appending an event to the durable inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    /// The event was newly and durably committed. Safe to ACK and process.
    Committed,
    /// The event was already present — a restart-safe idempotent replay.
    /// Safe to ACK, but must not be processed a second time.
    Duplicate,
}

impl AppendOutcome {
    /// True only when this call is the one that durably committed the event.
    #[must_use]
    pub fn is_committed(self) -> bool {
        matches!(self, Self::Committed)
    }
}

/// An event to persist. `event_id` is the idempotency key.
#[derive(Debug, Clone)]
pub struct InboxRecord {
    /// Stable idempotency key (op_pi derives this during normalization).
    pub event_id: String,
    /// Canonical event kind, e.g. `session.finished`.
    pub kind: String,
    /// The full normalized event payload.
    pub payload: Value,
}

impl EvidenceLedger {
    /// Durably append an event before it is acknowledged.
    ///
    /// Returns [`AppendOutcome::Committed`] the first time an `event_id` is seen
    /// and [`AppendOutcome::Duplicate`] on every redelivery, including after a
    /// process restart. The caller must only send its `200` once this returns
    /// `Ok`.
    pub async fn append(&self, record: &InboxRecord) -> Result<AppendOutcome, LedgerError> {
        let event_id = record.event_id.trim();
        if event_id.is_empty() {
            return Err(LedgerError::EmptyEventId);
        }

        let rows = sqlx::query(
            "INSERT INTO evidence_inbox (event_id, kind, payload) \
             VALUES ($1, $2, $3) ON CONFLICT (event_id) DO NOTHING",
        )
        .bind(event_id)
        .bind(&record.kind)
        .bind(Json(&record.payload))
        .execute(&self.pool)
        .await?
        .rows_affected();

        Ok(if rows == 1 {
            AppendOutcome::Committed
        } else {
            AppendOutcome::Duplicate
        })
    }

    /// Fetch a durably stored record by `event_id`.
    pub async fn get(&self, event_id: &str) -> Result<Option<InboxRecord>, LedgerError> {
        let row: Option<(String, String, Json<Value>)> = sqlx::query_as(
            "SELECT event_id, kind, payload FROM evidence_inbox WHERE event_id = $1",
        )
        .bind(event_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|(event_id, kind, payload)| InboxRecord {
            event_id,
            kind,
            payload: payload.0,
        }))
    }

    /// Count durably stored events.
    pub async fn count(&self) -> Result<i64, LedgerError> {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM evidence_inbox")
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }
}
