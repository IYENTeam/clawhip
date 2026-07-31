//! Durable evidence ledger for op_pi (AGI architecture §4.2).
//!
//! Today op_pi acknowledges a webhook with `200` as soon as the event lands in
//! an in-memory queue, so a crash or restart drops in-flight events and a
//! redelivery is processed twice. This crate closes that gap:
//!
//! - the **inbox** ([`InboxRecord`]) durably commits an event before it is
//!   acknowledged and deduplicates redeliveries on `event_id`;
//! - the **outbox** ([`NewOutboxEntry`]) is written in the same transaction as
//!   the inbox commit, so an event and its downstream intents are all-or-nothing
//!   and survive a restart until a relay drains them;
//! - the **outbox relay** ([`Dispatcher`]) drains those pending entries to their
//!   destinations at-least-once, stamping each only after a successful dispatch;
//! - the **accepted-receipt mirror** ([`AcceptedReceipt`]) is an append-only log
//!   of receipts Task Flow already accepted. op_pi mirrors and originates none.

mod error;
mod inbox;
mod intake;
mod mirror;
mod outbox;
mod relay;

use sqlx::PgPool;

pub use error::LedgerError;
pub use inbox::{AppendOutcome, InboxRecord};
pub use intake::IntakeError;
pub use mirror::{AcceptedReceipt, FORBIDDEN_ORIGINATION_KINDS, MirrorOutcome};
pub use outbox::{NewOutboxEntry, OutboxEntry};
pub use relay::{Dispatcher, RelayError, RelayProgress};

/// A PostgreSQL-backed durable evidence ledger.
#[derive(Clone)]
pub struct EvidenceLedger {
    pool: PgPool,
}

impl EvidenceLedger {
    /// Wrap an existing connection pool.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Connect to PostgreSQL and wrap the resulting pool.
    pub async fn connect(database_url: &str) -> Result<Self, LedgerError> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect(database_url)
            .await?;
        Ok(Self::new(pool))
    }

    /// Apply embedded migrations. Idempotent and safe to call on every start.
    pub async fn migrate(&self) -> Result<(), LedgerError> {
        sqlx::migrate!().run(&self.pool).await?;
        Ok(())
    }
}
