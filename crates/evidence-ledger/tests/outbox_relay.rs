//! Integration tests for the outbox relay drain (roadmap M2 — outbox→dispatcher).
//! PostgreSQL via `DATABASE_URL` is mandatory; missing or unreachable backends
//! fail the test binary.

mod support;

use std::sync::Mutex;

use evidence_ledger::{Dispatcher, EvidenceLedger, InboxRecord, NewOutboxEntry, OutboxEntry};
use serde_json::json;
use support::require_clean_pool;

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

/// Reads the ids of the currently-pending entries in dispatch order. Claiming
/// does not mutate the outbox, so this is a safe peek used to assert ordering.
async fn pending_ids(ledger: &EvidenceLedger) -> Vec<i64> {
    ledger
        .claim_pending_outbox(100)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.id)
        .collect()
}

/// A [`Dispatcher`] that records delivered entry ids in order and can be told to
/// fail on its Nth call to prove stall-and-retry behaviour.
#[derive(Default)]
struct RecordingDispatcher {
    delivered: Mutex<Vec<i64>>,
    calls: Mutex<usize>,
    fail_at_call: Option<usize>,
}

impl RecordingDispatcher {
    fn failing_at(call: usize) -> Self {
        Self {
            fail_at_call: Some(call),
            ..Self::default()
        }
    }

    fn delivered(&self) -> Vec<i64> {
        self.delivered.lock().unwrap().clone()
    }
}

impl Dispatcher for RecordingDispatcher {
    async fn dispatch(
        &self,
        entry: &OutboxEntry,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let call = {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            *calls
        };
        if self.fail_at_call == Some(call) {
            return Err("dispatch boom".into());
        }
        self.delivered.lock().unwrap().push(entry.id);
        Ok(())
    }
}

#[tokio::test]
#[serial_test::serial]
async fn relay_drains_pending_in_order_and_stamps_dispatched() {
    let pool = require_clean_pool().await;
    let ledger = EvidenceLedger::new(pool);
    ledger
        .accept(
            &record("evt-relay-order"),
            &[outbox("task-flow"), outbox("audit"), outbox("task-flow")],
        )
        .await
        .unwrap();
    let ids = pending_ids(&ledger).await;
    assert_eq!(ids.len(), 3);

    let dispatcher = RecordingDispatcher::default();
    let progress = ledger.relay_once(&dispatcher, 10).await.unwrap();

    assert_eq!(progress.dispatched, 3);
    assert!(!progress.stalled);
    // Delivered strictly in id (commit) order, and every entry is now stamped.
    assert_eq!(dispatcher.delivered(), ids);
    assert_eq!(ledger.pending_outbox_count().await.unwrap(), 0);

    // A second pass finds nothing and never re-delivers a stamped entry.
    let again = ledger.relay_once(&dispatcher, 10).await.unwrap();
    assert_eq!(again.dispatched, 0);
    assert!(!again.stalled);
    assert_eq!(dispatcher.delivered(), ids);
}

#[tokio::test]
#[serial_test::serial]
async fn relay_stalls_on_failure_then_retries_at_least_once() {
    let pool = require_clean_pool().await;
    let ledger = EvidenceLedger::new(pool);
    ledger
        .accept(
            &record("evt-relay-stall"),
            &[
                outbox("task-flow"),
                outbox("task-flow"),
                outbox("task-flow"),
            ],
        )
        .await
        .unwrap();
    let ids = pending_ids(&ledger).await;

    // Fails on the second delivery: the first is stamped, the tick then stops in
    // order without touching the third.
    let flaky = RecordingDispatcher::failing_at(2);
    let stalled = ledger.relay_once(&flaky, 10).await.unwrap();
    assert_eq!(stalled.dispatched, 1);
    assert!(stalled.stalled);
    assert_eq!(flaky.delivered(), vec![ids[0]]);
    assert_eq!(ledger.pending_outbox_count().await.unwrap(), 2);

    // A healthy retry drains exactly the two still-pending entries — the already
    // stamped first entry is never reclaimed (no duplicate delivery here).
    let healthy = RecordingDispatcher::default();
    let drained = ledger.relay_once(&healthy, 10).await.unwrap();
    assert_eq!(drained.dispatched, 2);
    assert!(!drained.stalled);
    assert_eq!(healthy.delivered(), vec![ids[1], ids[2]]);
    assert_eq!(ledger.pending_outbox_count().await.unwrap(), 0);
}

#[tokio::test]
#[serial_test::serial]
async fn drain_pending_walks_past_the_batch_window() {
    let pool = require_clean_pool().await;
    let ledger = EvidenceLedger::new(pool);
    ledger
        .accept(
            &record("evt-relay-drain"),
            &[
                outbox("task-flow"),
                outbox("task-flow"),
                outbox("task-flow"),
                outbox("task-flow"),
                outbox("task-flow"),
            ],
        )
        .await
        .unwrap();
    let ids = pending_ids(&ledger).await;
    assert_eq!(ids.len(), 5);

    // A batch of 2 forces multiple ticks; drain_pending must clear the backlog.
    let dispatcher = RecordingDispatcher::default();
    let progress = ledger.drain_pending(&dispatcher, 2).await.unwrap();

    assert_eq!(progress.dispatched, 5);
    assert!(!progress.stalled);
    assert_eq!(dispatcher.delivered(), ids);
    assert_eq!(ledger.pending_outbox_count().await.unwrap(), 0);
}

#[tokio::test]
#[serial_test::serial]
async fn relay_ignores_a_nonpositive_batch() {
    let pool = require_clean_pool().await;
    let ledger = EvidenceLedger::new(pool);
    ledger
        .accept(&record("evt-relay-zero"), &[outbox("task-flow")])
        .await
        .unwrap();

    // Zero is an empty window; a negative value would make Postgres' LIMIT error,
    // so the guard must turn both into a silent no-op — never a delivery, never
    // an error — leaving the entry pending.
    let dispatcher = RecordingDispatcher::default();
    for batch in [0, -1] {
        let progress = ledger.relay_once(&dispatcher, batch).await.unwrap();
        assert_eq!(progress.dispatched, 0);
        assert!(!progress.stalled);
    }
    assert!(dispatcher.delivered().is_empty());
    assert_eq!(ledger.pending_outbox_count().await.unwrap(), 1);
}

#[tokio::test]
#[serial_test::serial]
async fn drain_pending_stops_and_reports_a_stall() {
    let pool = require_clean_pool().await;
    let ledger = EvidenceLedger::new(pool);
    ledger
        .accept(
            &record("evt-relay-drain-stall"),
            &[
                outbox("task-flow"),
                outbox("task-flow"),
                outbox("task-flow"),
                outbox("task-flow"),
            ],
        )
        .await
        .unwrap();
    let ids = pending_ids(&ledger).await;

    // Batch 2: tick one drains ids[0..2]; tick two fails on the third delivery
    // (call 3) and leaves ids[2..4] pending.
    let flaky = RecordingDispatcher::failing_at(3);
    let progress = ledger.drain_pending(&flaky, 2).await.unwrap();

    assert_eq!(progress.dispatched, 2);
    assert!(progress.stalled);
    assert_eq!(flaky.delivered(), vec![ids[0], ids[1]]);
    assert_eq!(ledger.pending_outbox_count().await.unwrap(), 2);
}
