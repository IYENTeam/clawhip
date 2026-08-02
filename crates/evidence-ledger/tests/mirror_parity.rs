//! M1-W4 owner validator parity for the mirror family: run the AGI mirror
//! behavioral fixtures against the op_pi accepted-receipt mirror and require
//! identical verdicts. op_pi owns this family (vendored, AGI-authoritative
//! implementation), so this test is the owner-validator half of parity.
//!
//! Requires PostgreSQL via `DATABASE_URL`; missing or unreachable backends fail
//! closed. Fixtures load from `AGI_FIXTURES_DIR` (default: the enclosing AGI
//! monorepo).

mod support;

use std::path::PathBuf;

use evidence_ledger::{AcceptedReceipt, EvidenceLedger, LedgerError, MirrorOutcome};
use serde_json::{Value, json};
use sqlx::PgPool;
use support::require_clean_pool;

const DEFAULT_FIXTURES_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../contracts/fixtures"
);

async fn truncate(pool: &PgPool) {
    sqlx::query("TRUNCATE accepted_receipt_mirror RESTART IDENTITY CASCADE")
        .execute(pool)
        .await
        .expect("truncate mirror");
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("AGI_FIXTURES_DIR").unwrap_or_else(|_| DEFAULT_FIXTURES_DIR.to_string()),
    )
}

fn receipt(receipt_id: &str, body: Value) -> AcceptedReceipt {
    AcceptedReceipt {
        receipt_id: receipt_id.to_string(),
        run_id: "run-1".to_string(),
        kind: "task_flow.accepted".to_string(),
        body,
    }
}

/// Map one mirror fixture case to an owner verdict token.
async fn owner_verdict(ledger: &EvidenceLedger, pool: &PgPool, case: &Value) -> String {
    let id = case["id"].as_str().expect("case id");
    match id {
        "mirror-valid-accepted-receipt-append" => {
            let receipt_id = case["payload"]["receipt_id"].as_str().expect("receipt_id");
            let outcome = ledger
                .mirror_accepted(&receipt(receipt_id, json!({ "source": "task_flow" })))
                .await
                .expect("mirror append");
            if outcome == MirrorOutcome::Appended {
                "accept".to_string()
            } else {
                "fail_closed".to_string()
            }
        }
        "mirror-invalid-mutate-existing-receipt" => {
            let receipt_id = case["payload"]["receipt_id"].as_str().expect("receipt_id");
            ledger
                .mirror_accepted(&receipt(receipt_id, json!({ "body": "original" })))
                .await
                .expect("seed mirror row");
            // The only write path is append; a conflicting re-append must be
            // distinguished from an idempotent replay and fail closed.
            let outcome = ledger
                .mirror_accepted(&receipt(receipt_id, json!({ "body": "mutated" })))
                .await;
            let stored: Value = sqlx::query_scalar(
                "SELECT body FROM accepted_receipt_mirror WHERE receipt_id = $1",
            )
            .bind(receipt_id)
            .fetch_one(pool)
            .await
            .expect("stored body");
            if matches!(outcome, Err(LedgerError::PayloadMismatch { .. }))
                && stored["body"] == "original"
            {
                // The mutation was refused; the stored receipt is untouched.
                "reject".to_string()
            } else {
                "accept".to_string()
            }
        }
        "mirror-valid-mirror-only-no-origination" => {
            let outcome = ledger
                .mirror_accepted(&receipt(
                    "mirror-role-1",
                    json!({ "role": "mirror", "originates_authority": false }),
                ))
                .await
                .expect("mirror append");
            if outcome == MirrorOutcome::Appended {
                "accept".to_string()
            } else {
                "fail_closed".to_string()
            }
        }
        "mirror-invalid-op-pi-side-permit-write" => {
            // op_pi must not originate a selection, permit, or preference
            // write: the mirror guard refuses authority-origination kinds
            // fail-closed, and the schema carries no authority surface.
            let attempt = ledger
                .mirror_accepted(&AcceptedReceipt {
                    receipt_id: "origination-attempt".to_string(),
                    run_id: "run-1".to_string(),
                    kind: "effect_permit".to_string(),
                    body: json!({ "op": "write_effect_permit" }),
                })
                .await;
            let refused = matches!(attempt, Err(LedgerError::AuthorityOrigination));
            let tables: Vec<String> = sqlx::query_scalar(
                "SELECT table_name FROM information_schema.tables \
                 WHERE table_schema = 'public' AND table_name <> '_sqlx_migrations' \
                 ORDER BY table_name",
            )
            .fetch_all(pool)
            .await
            .expect("list tables");
            let no_authority_table = !tables.iter().any(|table| {
                table.contains("permit")
                    || table.contains("selection")
                    || table.contains("preference")
            });
            if refused && no_authority_table {
                "fail_closed".to_string()
            } else {
                "accept".to_string()
            }
        }
        other => panic!("unmapped mirror fixture case: {other}"),
    }
}

#[tokio::test]
#[serial_test::serial]
async fn mirror_fixtures_match_agi_verdicts() {
    let pool = require_clean_pool().await;
    let ledger = EvidenceLedger::new(pool.clone());

    let path = fixtures_dir().join("mirror.fixtures.json");
    let doc: Value = serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
    )
    .expect("parse mirror fixtures");
    assert_eq!(doc["schemaVersion"], "agi.conformance-fixtures.v1");
    assert_eq!(doc["family"], "mirror");
    assert_eq!(doc["authority"], "agi-behavioral");

    let cases = doc["cases"].as_array().expect("cases array");
    assert!(
        cases.len() >= 4,
        "mirror fixture matrix must cover both invariants"
    );

    let mut mismatches = Vec::new();
    for case in cases {
        truncate(&pool).await;
        let id = case["id"].as_str().expect("case id");
        let expected = case["expect"].as_str().expect("expect");
        let actual = owner_verdict(&ledger, &pool, case).await;
        if actual != expected {
            mismatches.push(format!("{id}: owner={actual} agi={expected}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "mirror parity mismatches: {mismatches:?}"
    );
}
