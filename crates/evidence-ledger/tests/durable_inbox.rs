//! Integration tests for the durable evidence inbox.
//!
//! Requires a live PostgreSQL reachable via `DATABASE_URL`; tests no-op when it
//! is unset so the suite stays green in environments without a database.

use evidence_ledger::{AppendOutcome, EvidenceLedger, InboxRecord, LedgerError};
use serde_json::json;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

async fn ledger() -> Option<EvidenceLedger> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("connect to DATABASE_URL");
    let ledger = EvidenceLedger::new(pool.clone());
    ledger.migrate().await.expect("run migrations");
    truncate(&pool).await;
    Some(ledger)
}

async fn truncate(pool: &PgPool) {
    sqlx::query("TRUNCATE evidence_inbox")
        .execute(pool)
        .await
        .expect("truncate");
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
    let Some(ledger) = ledger().await else {
        return;
    };
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
    let Some(ledger) = ledger().await else {
        return;
    };

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
    let url = match std::env::var("DATABASE_URL") {
        Ok(url) => url,
        Err(_) => return,
    };
    // First "process": commit the event, then drop the pool to simulate a crash.
    let first = PgPoolOptions::new().connect(&url).await.unwrap();
    let ledger = EvidenceLedger::new(first.clone());
    ledger.migrate().await.unwrap();
    truncate(&first).await;
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
    let Some(ledger) = ledger().await else {
        return;
    };
    let err = ledger.append(&record("   ", "blank")).await.unwrap_err();
    assert!(matches!(err, LedgerError::EmptyEventId));
    assert_eq!(ledger.count().await.unwrap(), 0);
}

#[tokio::test]
#[serial_test::serial]
async fn interleaved_replays_keep_exactly_one_row() {
    let Some(ledger) = ledger().await else {
        return;
    };
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
