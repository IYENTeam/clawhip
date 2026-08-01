//! Typed errors for the durable evidence ledger.

use thiserror::Error;

/// Errors surfaced by the durable ledger.
#[derive(Debug, Error)]
pub enum LedgerError {
    /// A database operation failed.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    /// A migration operation failed.
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// The event carried no usable `event_id`, so it cannot be deduplicated.
    #[error("event_id must be a non-empty string")]
    EmptyEventId,
    /// An outbox entry had an empty destination.
    #[error("outbox destination must be a non-empty string")]
    EmptyDestination,
    /// A receipt carried no usable `receipt_id`.
    #[error("receipt_id must be a non-empty string")]
    EmptyReceiptId,
    /// An idempotency key was replayed with different immutable content.
    #[error("duplicate {record_type} {record_id} has a different payload")]
    PayloadMismatch {
        /// The durable record family (`inbox` or `mirror`).
        record_type: &'static str,
        /// The conflicting idempotency key.
        record_id: String,
    },
    /// The mirror was asked to record an authority-origination kind
    /// (ADR-011 no-authority-origination): op_pi mirrors decisions made by
    /// Task Flow and originates none.
    #[error("mirror cannot originate a selection, permit, or preference write")]
    AuthorityOrigination,
}
