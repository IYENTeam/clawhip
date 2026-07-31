//! Append-only mirror of Task-Flow-accepted receipts.
//!
//! ADR-011 invariants: `append-only-accepted` (a stored receipt is never
//! rewritten) and `no-authority-origination` (op_pi only mirrors decisions made
//! by Task Flow; there is deliberately no API here to originate a selection,
//! permit, or preference).

use serde_json::Value;
use sqlx::types::Json;

use crate::{EvidenceLedger, LedgerError};

/// Result of mirroring an accepted receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorOutcome {
    /// The receipt was newly appended.
    Appended,
    /// The receipt was already mirrored — replay-safe, no rewrite.
    AlreadyMirrored,
}

/// A receipt Task Flow has already accepted, to be mirrored verbatim.
#[derive(Debug, Clone)]
pub struct AcceptedReceipt {
    /// Stable receipt id issued by Task Flow.
    pub receipt_id: String,
    /// The run the receipt belongs to.
    pub run_id: String,
    /// Receipt kind.
    pub kind: String,
    /// Opaque receipt body as accepted upstream.
    pub body: Value,
}

impl EvidenceLedger {
    /// Append a Task-Flow-accepted receipt to the mirror.
    ///
    /// Append-only: an existing `receipt_id` is never rewritten, so replays are
    /// absorbed as [`MirrorOutcome::AlreadyMirrored`].
    pub async fn mirror_accepted(
        &self,
        receipt: &AcceptedReceipt,
    ) -> Result<MirrorOutcome, LedgerError> {
        let receipt_id = receipt.receipt_id.trim();
        if receipt_id.is_empty() {
            return Err(LedgerError::EmptyReceiptId);
        }

        let rows = sqlx::query(
            "INSERT INTO accepted_receipt_mirror (receipt_id, run_id, kind, body) \
             VALUES ($1, $2, $3, $4) ON CONFLICT (receipt_id) DO NOTHING",
        )
        .bind(receipt_id)
        .bind(&receipt.run_id)
        .bind(&receipt.kind)
        .bind(Json(&receipt.body))
        .execute(&self.pool)
        .await?
        .rows_affected();

        Ok(if rows == 1 {
            MirrorOutcome::Appended
        } else {
            MirrorOutcome::AlreadyMirrored
        })
    }

    /// Count mirrored receipts.
    pub async fn count_mirrored(&self) -> Result<i64, LedgerError> {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM accepted_receipt_mirror")
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }
}
