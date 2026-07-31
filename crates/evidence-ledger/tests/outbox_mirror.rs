//! Integration tests for the transactional outbox and the accepted-receipt
//! mirror (roadmap M2-W3/W4). Requires PostgreSQL via `DATABASE_URL`; no-ops
//! when unset.

use evidence_ledger::{
    AcceptedReceipt, AppendOutcome, EvidenceLedger, InboxRecord, LedgerError, MirrorOutcome,
    NewOutboxEntry,
};
use serde_json::json;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

async fn pool() -> Option<PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("connect to DATABASE_URL");
    EvidenceLedger::new(pool.clone())
        .migrate()
        .await
        .expect("run migrations");
    sqlx::query("TRUNCATE evidence_inbox, evidence_outbox, accepted_receipt_mirror RESTART IDENTITY CASCADE")
        .execute(&pool)
        .await
        .expect("truncate");
    Some(pool)
}

fn record(event_id: &str) -> InboxRecord {
    InboxRecord {
        event_id: event_id.to_string(),
        kind: "session.finished".to_string(),
        payload: json!({ "event_id": event_id }),
    }
}

fn outbox(destination: &str) -> NewOutboxEntry {
    NewOutboxEntry {
        destination: destination.to_string(),
        payload: json!({ "to": destination }),
    }
}

#[tokio::test]
#[serial_test::serial]
async fn accept_commits_inbox_and_outbox_atomically() {
    let Some(pool) = pool().await else { return };
    let ledger = EvidenceLedger::new(pool);
    let outcome = ledger
        .accept(&record("evt-A"), &[outbox("task-flow"), outbox("audit")])
        .await
        .unwrap();
    assert_eq!(outcome, AppendOutcome::Committed);
    assert_eq!(ledger.count().await.unwrap(), 1);
    assert_eq!(ledger.pending_outbox_count().await.unwrap(), 2);
}

#[tokio::test]
#[serial_test::serial]
async fn duplicate_accept_keeps_outbox_idempotent() {
    let Some(pool) = pool().await else { return };
    let ledger = EvidenceLedger::new(pool);
    let entries = [outbox("task-flow")];
    assert_eq!(
        ledger.accept(&record("evt-B"), &entries).await.unwrap(),
        AppendOutcome::Committed
    );
    assert_eq!(
        ledger.accept(&record("evt-B"), &entries).await.unwrap(),
        AppendOutcome::Duplicate
    );
    assert_eq!(ledger.pending_outbox_count().await.unwrap(), 1);
}

#[tokio::test]
#[serial_test::serial]
async fn relay_claims_then_marks_dispatched() {
    let Some(pool) = pool().await else { return };
    let ledger = EvidenceLedger::new(pool);
    ledger
        .accept(&record("evt-C"), &[outbox("task-flow"), outbox("audit")])
        .await
        .unwrap();

    let pending = ledger.claim_pending_outbox(10).await.unwrap();
    assert_eq!(pending.len(), 2);
    for entry in &pending {
        assert!(ledger.mark_outbox_dispatched(entry.id).await.unwrap());
    }
    // Second mark is a no-op, and nothing remains pending.
    assert!(!ledger.mark_outbox_dispatched(pending[0].id).await.unwrap());
    assert_eq!(ledger.pending_outbox_count().await.unwrap(), 0);
    assert!(ledger.claim_pending_outbox(10).await.unwrap().is_empty());
}

#[tokio::test]
#[serial_test::serial]
async fn pending_outbox_survives_a_restart() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(url) => url,
        Err(_) => return,
    };
    let first = pool().await.unwrap();
    let ledger = EvidenceLedger::new(first.clone());
    ledger
        .accept(&record("evt-D"), &[outbox("task-flow")])
        .await
        .unwrap();
    first.close().await;

    let second = PgPoolOptions::new().connect(&url).await.unwrap();
    let restarted = EvidenceLedger::new(second);
    assert_eq!(restarted.pending_outbox_count().await.unwrap(), 1);
    assert_eq!(restarted.claim_pending_outbox(10).await.unwrap().len(), 1);
}

#[tokio::test]
#[serial_test::serial]
async fn rolled_back_transaction_leaves_no_partial_write() {
    // Proves the atomicity `accept` relies on: a failed transaction writes
    // neither the inbox row nor the outbox row (loss 0, no orphan).
    let Some(pool) = pool().await else { return };
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO evidence_inbox (event_id, kind, payload) VALUES ($1, $2, $3)")
        .bind("evt-rollback")
        .bind("k")
        .bind(json!({}))
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO evidence_outbox (event_id, destination, payload) VALUES ($1, $2, $3)")
        .bind("evt-rollback")
        .bind("task-flow")
        .bind(json!({}))
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();

    let ledger = EvidenceLedger::new(pool);
    assert_eq!(ledger.count().await.unwrap(), 0);
    assert_eq!(ledger.pending_outbox_count().await.unwrap(), 0);
}

#[tokio::test]
#[serial_test::serial]
async fn mirror_is_append_only_and_replay_safe() {
    let Some(pool) = pool().await else { return };
    let ledger = EvidenceLedger::new(pool);
    let receipt = AcceptedReceipt {
        receipt_id: "rcpt-1".to_string(),
        run_id: "run-9".to_string(),
        kind: "effect.accepted".to_string(),
        body: json!({ "ok": true }),
    };
    assert_eq!(
        ledger.mirror_accepted(&receipt).await.unwrap(),
        MirrorOutcome::Appended
    );
    // A different body under the same receipt_id must NOT overwrite the row.
    let tampered = AcceptedReceipt {
        body: json!({ "ok": false }),
        ..receipt.clone()
    };
    assert_eq!(
        ledger.mirror_accepted(&tampered).await.unwrap(),
        MirrorOutcome::AlreadyMirrored
    );
    assert_eq!(ledger.count_mirrored().await.unwrap(), 1);
}

#[tokio::test]
#[serial_test::serial]
async fn empty_destination_and_receipt_id_fail_closed() {
    let Some(pool) = pool().await else { return };
    let ledger = EvidenceLedger::new(pool);

    let bad_outbox = ledger
        .accept(&record("evt-E"), &[outbox("  ")])
        .await
        .unwrap_err();
    assert!(matches!(bad_outbox, LedgerError::EmptyDestination));
    // The whole accept failed closed: nothing was written.
    assert_eq!(ledger.count().await.unwrap(), 0);
    assert_eq!(ledger.pending_outbox_count().await.unwrap(), 0);

    let bad_receipt = AcceptedReceipt {
        receipt_id: "   ".to_string(),
        run_id: "run".to_string(),
        kind: "k".to_string(),
        body: json!({}),
    };
    let err = ledger.mirror_accepted(&bad_receipt).await.unwrap_err();
    assert!(matches!(err, LedgerError::EmptyReceiptId));
    assert_eq!(ledger.count_mirrored().await.unwrap(), 0);
}

#[tokio::test]
#[serial_test::serial]
async fn accept_fails_closed_during_a_db_outage() {
    let Some(healthy) = pool().await else { return };
    let ledger = EvidenceLedger::new(healthy);

    // Sever a second connection to the same database to simulate an outage. The
    // transactional accept cannot even begin, so it fails closed: no inbox row,
    // no outbox row, no false ACK.
    let url = std::env::var("DATABASE_URL").unwrap();
    let severed = PgPoolOptions::new().connect(&url).await.unwrap();
    let during_outage = EvidenceLedger::new(severed.clone());
    severed.close().await;
    let err = during_outage
        .accept(&record("evt-outage-accept"), &[outbox("task-flow")])
        .await
        .unwrap_err();
    assert!(matches!(err, LedgerError::Database(_)));
    assert_eq!(ledger.count().await.unwrap(), 0);
    assert_eq!(ledger.pending_outbox_count().await.unwrap(), 0);

    // Recovery: the redelivered event commits atomically, exactly once, with its
    // outbox intent intact.
    assert_eq!(
        ledger
            .accept(&record("evt-outage-accept"), &[outbox("task-flow")])
            .await
            .unwrap(),
        AppendOutcome::Committed
    );
    assert_eq!(ledger.count().await.unwrap(), 1);
    assert_eq!(ledger.pending_outbox_count().await.unwrap(), 1);
}
