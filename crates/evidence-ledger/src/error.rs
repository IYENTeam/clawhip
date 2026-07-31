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
}
