//! Stable payload identities used to bind idempotency keys to content.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(crate) fn inbox_hash(kind: &str, payload: &Value) -> String {
    hash(&json!(["evidence_inbox.v1", kind, payload]))
}

pub(crate) fn accept_hash<'a>(
    kind: &str,
    payload: &Value,
    outbox: impl IntoIterator<Item = (&'a str, &'a Value)>,
) -> String {
    let intents: Vec<Value> = outbox
        .into_iter()
        .map(|(destination, value)| json!([destination.trim(), value]))
        .collect();
    hash(&json!(["transactional_accept.v1", kind, payload, intents]))
}

pub(crate) fn mirror_hash(run_id: &str, kind: &str, body: &Value) -> String {
    hash(&json!(["accepted_receipt_mirror.v1", run_id, kind, body]))
}

fn hash(value: &Value) -> String {
    let encoded = serde_json::to_vec(value).expect("serde_json::Value serialization is infallible");
    format!("{:x}", Sha256::digest(encoded))
}
