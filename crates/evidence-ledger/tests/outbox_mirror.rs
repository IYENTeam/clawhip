//! Integration tests for the transactional outbox and the accepted-receipt
//! mirror (roadmap M2-W3/W4). PostgreSQL via `DATABASE_URL` is mandatory;
//! missing or unreachable backends fail the test binary.

mod support;

use evidence_ledger::{
    AcceptedReceipt, AppendOutcome, EvidenceLedger, InboxRecord, LedgerError, MirrorOutcome,
    NewOutboxEntry,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use support::{require_clean_pool, require_database_url};

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
    let pool = require_clean_pool().await;
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
    let pool = require_clean_pool().await;
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
    let pool = require_clean_pool().await;
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
    let url = require_database_url();
    let first = require_clean_pool().await;
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
    let pool = require_clean_pool().await;
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
    let pool = require_clean_pool().await;
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
    let pool = require_clean_pool().await;
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
    let healthy = require_clean_pool().await;
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

#[tokio::test]
#[serial_test::serial]
async fn mirror_refuses_authority_origination_kinds() {
    let pool = require_clean_pool().await;
    let ledger = EvidenceLedger::new(pool);

    // ADR-011 no-authority-origination: op_pi mirrors decisions made by
    // Task Flow and originates none. Raw selection/permit/preference writes
    // are refused fail-closed and leave no mirror row.
    for kind in evidence_ledger::FORBIDDEN_ORIGINATION_KINDS {
        let err = ledger
            .mirror_accepted(&AcceptedReceipt {
                receipt_id: format!("origination-attempt-{kind}"),
                run_id: "run-1".to_string(),
                kind: (*kind).to_string(),
                body: json!({ "originated_by": "op_pi" }),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, LedgerError::AuthorityOrigination));
    }
    assert_eq!(ledger.count_mirrored().await.unwrap(), 0);

    // A genuine Task-Flow-accepted receipt still mirrors.
    assert_eq!(
        ledger
            .mirror_accepted(&AcceptedReceipt {
                receipt_id: "accepted-1".to_string(),
                run_id: "run-1".to_string(),
                kind: "task_flow.accepted".to_string(),
                body: json!({ "decision": "made-upstream" }),
            })
            .await
            .unwrap(),
        MirrorOutcome::Appended
    );
    assert_eq!(ledger.count_mirrored().await.unwrap(), 1);
}
