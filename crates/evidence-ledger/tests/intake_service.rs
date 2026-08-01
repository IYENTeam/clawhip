//! Integration tests for the intake wiring (durable-before-signal).
//!
//! PostgreSQL via `DATABASE_URL` is mandatory; missing or unreachable backends
//! fail the test binary.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use evidence_ledger::{AppendOutcome, EvidenceLedger, InboxRecord, IntakeError, NewOutboxEntry};
use serde_json::json;
use support::require_clean_pool;

async fn ledger() -> EvidenceLedger {
    EvidenceLedger::new(require_clean_pool().await)
}

fn record(event_id: &str) -> InboxRecord {
    InboxRecord {
        event_id: event_id.to_string(),
        kind: "session.finished".to_string(),
        payload: json!({ "event_id": event_id }),
    }
}

fn outbox() -> Vec<NewOutboxEntry> {
    vec![NewOutboxEntry {
        destination: "task-flow".to_string(),
        payload: json!({}),
    }]
}

#[tokio::test]
#[serial_test::serial]
async fn signals_once_on_commit_and_never_on_duplicate() {
    let ledger = ledger().await;
    let signals = Arc::new(AtomicUsize::new(0));

    let first = ledger
        .accept_and_signal(&record("evt-A"), &outbox(), || {
            let signals = Arc::clone(&signals);
            async move {
                signals.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .await
        .unwrap();
    assert_eq!(first, AppendOutcome::Committed);

    let second = ledger
        .accept_and_signal(&record("evt-A"), &outbox(), || {
            let signals = Arc::clone(&signals);
            async move {
                signals.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .await
        .unwrap();
    assert_eq!(second, AppendOutcome::Duplicate);

    // Committed once, so the queue was signaled exactly once.
    assert_eq!(signals.load(Ordering::SeqCst), 1);
    assert_eq!(ledger.count().await.unwrap(), 1);
}

#[tokio::test]
#[serial_test::serial]
async fn event_stays_durable_when_signal_fails() {
    let ledger = ledger().await;

    let result = ledger
        .accept_and_signal(&record("evt-B"), &outbox(), || async {
            Err("queue closed".into())
        })
        .await;

    // The signal failed, so the caller must not send its 200...
    assert!(matches!(result, Err(IntakeError::Sink(_))));
    // ...but the event is durably stored, ready for the outbox relay / redelivery.
    assert_eq!(ledger.count().await.unwrap(), 1);
    assert_eq!(ledger.pending_outbox_count().await.unwrap(), 1);
}

#[tokio::test]
#[serial_test::serial]
async fn empty_event_id_never_signals() {
    let ledger = ledger().await;
    let signals = Arc::new(AtomicUsize::new(0));

    let result = ledger
        .accept_and_signal(&record("   "), &outbox(), || {
            let signals = Arc::clone(&signals);
            async move {
                signals.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .await;

    assert!(matches!(result, Err(IntakeError::Ledger(_))));
    assert_eq!(signals.load(Ordering::SeqCst), 0);
    assert_eq!(ledger.count().await.unwrap(), 0);
}
