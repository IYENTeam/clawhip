//! Integration tests for the durable evidence inbox.
//!
//! Requires a live PostgreSQL reachable via `DATABASE_URL`. Missing or
//! unreachable backends fail closed instead of turning evidence tests into no-ops.

mod support;

use evidence_ledger::{AppendOutcome, EvidenceLedger, InboxRecord, LedgerError};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use support::{require_clean_pool, require_database_url};

async fn ledger() -> EvidenceLedger {
    EvidenceLedger::new(require_clean_pool().await)
}

fn record(event_id: &str, kind: &str) -> InboxRecord {
    InboxRecord {
        event_id: event_id.to_string(),
        kind: kind.to_string(),
        payload: json!({ "event_id": event_id, "kind": kind }),
    }
}

#[tokio::test]
#[serial_test::serial]
async fn append_commits_then_deduplicates() {
    let ledger = ledger().await;
    let event = record("evt-A", "session.finished");

    assert_eq!(
        ledger.append(&event).await.unwrap(),
        AppendOutcome::Committed
    );
    assert_eq!(
        ledger.append(&event).await.unwrap(),
        AppendOutcome::Duplicate
    );
    assert_eq!(
        ledger.append(&event).await.unwrap(),
        AppendOutcome::Duplicate
    );

    let stored = ledger.get("evt-A").await.unwrap().expect("stored");
    assert_eq!(stored.kind, "session.finished");
    assert_eq!(ledger.count().await.unwrap(), 1);
}

#[tokio::test]
#[serial_test::serial]
async fn distinct_event_ids_each_commit() {
    let ledger = ledger().await;

    assert!(
        ledger
            .append(&record("evt-1", "a"))
            .await
            .unwrap()
            .is_committed()
    );
    assert!(
        ledger
            .append(&record("evt-2", "b"))
            .await
            .unwrap()
            .is_committed()
    );
    assert_eq!(ledger.count().await.unwrap(), 2);
}

#[tokio::test]
#[serial_test::serial]
async fn dedupe_survives_a_restart() {
    let url = require_database_url();
    // First "process": commit the event, then drop the pool to simulate a crash.
    let first = require_clean_pool().await;
    let ledger = EvidenceLedger::new(first.clone());
    assert_eq!(
        ledger.append(&record("evt-restart", "x")).await.unwrap(),
        AppendOutcome::Committed
    );
    drop(ledger);
    first.close().await;

    // Second "process": a brand-new pool must still see the event as durable.
    let second = PgPoolOptions::new().connect(&url).await.unwrap();
    let restarted = EvidenceLedger::new(second);
    assert_eq!(
        restarted.append(&record("evt-restart", "x")).await.unwrap(),
        AppendOutcome::Duplicate
    );
    assert_eq!(restarted.count().await.unwrap(), 1);
}

#[tokio::test]
#[serial_test::serial]
async fn empty_event_id_is_rejected() {
    let ledger = ledger().await;
    let err = ledger.append(&record("   ", "blank")).await.unwrap_err();
    assert!(matches!(err, LedgerError::EmptyEventId));
    assert_eq!(ledger.count().await.unwrap(), 0);
}

#[tokio::test]
#[serial_test::serial]
async fn interleaved_replays_keep_exactly_one_row() {
    let ledger = ledger().await;
    // A fixed id replayed many times, interleaved with unique ids. The fixed id
    // must end with exactly one row regardless of order.
    let mut committed_unique = 0i64;
    for index in 0..50 {
        assert!(matches!(
            ledger.append(&record("evt-fixed", "fixed")).await.unwrap(),
            AppendOutcome::Committed | AppendOutcome::Duplicate
        ));
        if index % 5 == 0 {
            let unique = format!("evt-{index}");
            assert_eq!(
                ledger.append(&record(&unique, "uniq")).await.unwrap(),
                AppendOutcome::Committed
            );
            committed_unique += 1;
        }
    }

    assert!(ledger.get("evt-fixed").await.unwrap().is_some());
    assert_eq!(ledger.count().await.unwrap(), committed_unique + 1);
}

#[tokio::test]
#[serial_test::serial]
async fn outage_fails_closed_then_keeps_loss_zero_after_recovery() {
    let url = require_database_url();
    // A healthy pool owns the schema and the clean slate; a second, severed pool
    // stands in for the same database during an outage.
    let healthy = require_clean_pool().await;
    let ledger = EvidenceLedger::new(healthy.clone());

    let severed = PgPoolOptions::new().connect(&url).await.unwrap();
    let during_outage = EvidenceLedger::new(severed.clone());
    severed.close().await;

    // Commit-before-ACK under an outage: append fails closed (an error, so the
    // caller withholds its 200) and durably writes nothing.
    let err = during_outage
        .append(&record("evt-outage", "session.finished"))
        .await
        .unwrap_err();
    assert!(matches!(err, LedgerError::Database(_)));
    assert!(ledger.get("evt-outage").await.unwrap().is_none());
    assert_eq!(ledger.count().await.unwrap(), 0);

    // Recovery + redelivery: the event commits exactly once and a further
    // redelivery deduplicates — loss 0 and no double-processing across the outage.
    assert_eq!(
        ledger
            .append(&record("evt-outage", "session.finished"))
            .await
            .unwrap(),
        AppendOutcome::Committed
    );
    assert_eq!(
        ledger
            .append(&record("evt-outage", "session.finished"))
            .await
            .unwrap(),
        AppendOutcome::Duplicate
    );
    assert_eq!(ledger.count().await.unwrap(), 1);
}
