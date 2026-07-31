//! Intake wiring: durably accept an event, then signal downstream once.
//!
//! This is the ordering the live `POST /linear` handler must follow — commit to
//! the durable inbox first, and only signal the in-memory queue (the `200`
//! path) after the commit succeeds and only on the first delivery. A redelivery
//! is acknowledged without re-signaling; a signal failure after a durable commit
//! is not a loss because the row is already stored for the outbox relay to pick
//! up.

use std::future::Future;

use thiserror::Error;

use crate::{AppendOutcome, EvidenceLedger, InboxRecord, LedgerError, NewOutboxEntry};

/// Errors from the intake wiring.
#[derive(Debug, Error)]
pub enum IntakeError {
    /// The durable commit failed; nothing was signaled.
    #[error("ledger: {0}")]
    Ledger(#[from] LedgerError),
    /// The event was durably committed but the downstream signal failed.
    #[error("sink: {0}")]
    Sink(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl EvidenceLedger {
    /// Durably accept an event, then run `on_committed` exactly once for a
    /// first-time commit.
    ///
    /// - [`AppendOutcome::Committed`]: the event was durably stored and
    ///   `on_committed` (e.g. enqueue for processing) ran.
    /// - [`AppendOutcome::Duplicate`]: a redelivery — acknowledged without
    ///   running `on_committed` again.
    ///
    /// If `on_committed` fails the event stays durably stored (no loss); the
    /// error is surfaced as [`IntakeError::Sink`] so the caller can withhold its
    /// `200` and let the redelivery or outbox relay retry.
    pub async fn accept_and_signal<F, Fut>(
        &self,
        record: &InboxRecord,
        outbox: &[NewOutboxEntry],
        on_committed: F,
    ) -> Result<AppendOutcome, IntakeError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>>,
    {
        let outcome = self.accept(record, outbox).await?;
        if matches!(outcome, AppendOutcome::Committed) {
            on_committed().await.map_err(IntakeError::Sink)?;
        }
        Ok(outcome)
    }
}
