//! PostgreSQL lease and failure bookkeeping for concurrent outbox drainers.

use serde_json::Value;
use sqlx::types::Json;

use crate::{EvidenceLedger, LedgerError, OutboxEntry};

pub(crate) const MAX_DISPATCH_ATTEMPTS: i32 = 3;
const LEASE_SECONDS: f64 = 30.0;

impl EvidenceLedger {
    pub(crate) async fn lease_pending_outbox(
        &self,
        limit: i64,
        lease_id: &str,
    ) -> Result<Vec<OutboxEntry>, LedgerError> {
        let rows: Vec<(i64, String, String, Json<Value>)> = sqlx::query_as(
            "WITH candidates AS (\
               SELECT id FROM evidence_outbox \
               WHERE dispatched_at IS NULL AND dead_lettered_at IS NULL \
                 AND (lease_expires_at IS NULL OR lease_expires_at <= now()) \
               ORDER BY id FOR UPDATE SKIP LOCKED LIMIT $1\
             ) \
             UPDATE evidence_outbox AS entry \
             SET lease_token = $2, \
                 lease_expires_at = now() + make_interval(secs => $3) \
             FROM candidates WHERE entry.id = candidates.id \
             RETURNING entry.id, entry.event_id, entry.destination, entry.payload",
        )
        .bind(limit)
        .bind(lease_id)
        .bind(LEASE_SECONDS)
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

    pub(crate) async fn mark_leased_outbox_dispatched(
        &self,
        id: i64,
        lease_id: &str,
    ) -> Result<bool, LedgerError> {
        let rows = sqlx::query(
            "UPDATE evidence_outbox SET dispatched_at = now(), lease_token = NULL, \
             lease_expires_at = NULL WHERE id = $1 AND dispatched_at IS NULL \
             AND dead_lettered_at IS NULL AND lease_token = $2",
        )
        .bind(id)
        .bind(lease_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(rows == 1)
    }

    pub(crate) async fn record_dispatch_failure(
        &self,
        id: i64,
        lease_id: &str,
    ) -> Result<bool, LedgerError> {
        let dead_lettered: Option<bool> = sqlx::query_scalar(
            "UPDATE evidence_outbox SET \
               attempt_count = attempt_count + 1, \
               dead_lettered_at = CASE WHEN attempt_count + 1 >= $3 THEN now() ELSE NULL END, \
               lease_token = NULL, lease_expires_at = NULL \
             WHERE id = $1 AND dispatched_at IS NULL \
               AND dead_lettered_at IS NULL AND lease_token = $2 \
             RETURNING dead_lettered_at IS NOT NULL",
        )
        .bind(id)
        .bind(lease_id)
        .bind(MAX_DISPATCH_ATTEMPTS)
        .fetch_optional(&self.pool)
        .await?;
        Ok(dead_lettered.unwrap_or(false))
    }
}
