//! Outbox relay: drain durably-committed outbox entries to their destinations.
//!
//! The transactional outbox ([`crate::NewOutboxEntry`]) stores every downstream
//! intent in the same commit as its inbox row, so an accepted event and its
//! intents are all-or-nothing and survive a restart. This relay is the other
//! half: it claims the oldest pending entries, hands each to a [`Dispatcher`],
//! and stamps `dispatched_at` only after that dispatch succeeds.
//!
//! Delivery is therefore **at-least-once** — a crash between a successful
//! dispatch and its stamp replays the entry, and the downstream dedupes. Drain
//! workers claim disjoint rows with expiring PostgreSQL leases. A dispatch failure
//! is durably counted and does not block later rows; after three failures the row
//! is quarantined as a dead letter rather than silently skipped.
//!
//! A dispatch failure is normal backpressure, not an error: [`relay_once`] still
//! returns `Ok` and records it in [`RelayProgress`]. Only a ledger (database)
//! failure surfaces as [`RelayError`].
//!
//! [`relay_once`]: EvidenceLedger::relay_once

use std::future::Future;

use thiserror::Error;
use uuid::Uuid;

use crate::{EvidenceLedger, LedgerError, OutboxEntry};

/// A sink that delivers one outbox entry to its destination.
pub trait Dispatcher {
    /// Deliver `entry`. Returning `Ok(())` authorizes the relay to stamp the
    /// entry dispatched; an `Err` leaves it pending for a later retry and stalls
    /// the current tick.
    fn dispatch(
        &self,
        entry: &OutboxEntry,
    ) -> impl Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send;
}

/// What a relay tick (or a full drain) accomplished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RelayProgress {
    /// Entries delivered and durably stamped dispatched.
    pub dispatched: u64,
    /// Dispatch attempts that failed and were durably recorded.
    pub failed_attempts: u64,
    /// Entries quarantined after exhausting their retry budget.
    pub dead_lettered: u64,
    /// True when at least one dispatch attempt failed. Later rows still run.
    pub stalled: bool,
}

/// Errors from the relay. A dispatch failure is *not* one of these — it is
/// expected backpressure surfaced through [`RelayProgress::stalled`]. Only a
/// ledger (database) failure aborts a tick.
#[derive(Debug, Error)]
pub enum RelayError {
    /// Claiming or stamping an outbox entry failed.
    #[error("ledger: {0}")]
    Ledger(#[from] LedgerError),
}

impl EvidenceLedger {
    /// Drain up to `batch` pending outbox entries through `dispatcher` once.
    ///
    /// Entries are delivered in `id` order. Each successful dispatch is stamped
    /// before the next entry is attempted, so a stamp always reflects a real
    /// delivery. The tick stops at the first dispatch failure and reports
    /// [`RelayProgress::stalled`], leaving the failed entry pending.
    ///
    /// A non-positive `batch` is a no-op that dispatches nothing — the guard
    /// keeps a misconfigured window from reaching Postgres, whose `LIMIT`
    /// rejects a negative value outright.
    pub async fn relay_once<D>(
        &self,
        dispatcher: &D,
        batch: i64,
    ) -> Result<RelayProgress, RelayError>
    where
        D: Dispatcher,
    {
        if batch <= 0 {
            return Ok(RelayProgress::default());
        }
        let lease_id = Uuid::new_v4().to_string();
        let pending = self.lease_pending_outbox(batch, &lease_id).await?;
        let mut progress = RelayProgress::default();
        for entry in &pending {
            if dispatcher.dispatch(entry).await.is_err() {
                progress.failed_attempts += 1;
                progress.stalled = true;
                if self.record_dispatch_failure(entry.id, &lease_id).await? {
                    progress.dead_lettered += 1;
                }
                continue;
            }
            if self
                .mark_leased_outbox_dispatched(entry.id, &lease_id)
                .await?
            {
                progress.dispatched += 1;
            }
        }
        Ok(progress)
    }

    /// Repeatedly [`relay_once`] until the backlog is empty or a dispatch stalls.
    ///
    /// This is the full drain a scheduler calls on a tick: it walks past the
    /// `batch` claim window so a large backlog is delivered in one pass. It stops
    /// as soon as a tick makes no further progress — either nothing remains, or a
    /// dispatch failed (`stalled`) — and returns the aggregate.
    ///
    /// [`relay_once`]: EvidenceLedger::relay_once
    pub async fn drain_pending<D>(
        &self,
        dispatcher: &D,
        batch: i64,
    ) -> Result<RelayProgress, RelayError>
    where
        D: Dispatcher,
    {
        let mut total = RelayProgress::default();
        loop {
            let tick = self.relay_once(dispatcher, batch).await?;
            total.dispatched += tick.dispatched;
            total.failed_attempts += tick.failed_attempts;
            total.dead_lettered += tick.dead_lettered;
            total.stalled |= tick.stalled;
            if tick.dispatched == 0 && tick.failed_attempts == 0 {
                return Ok(total);
            }
        }
    }
}
