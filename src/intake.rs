//! External intake normalization for AWS and Cloudflare webhooks.
//!
//! Each source has a pure `normalize_*` function that maps the provider's
//! wire format into an [`IncomingEvent`] following clawhip's event model, so
//! daemon handlers stay thin and the mapping is unit-testable.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine as _;
use rsa::RsaPublicKey;
use serde_json::{Value, json};

use crate::events::IncomingEvent;

/// Maximum number of Logpush records embedded in a single batch event.
pub const LOGPUSH_RECORD_CAP: usize = 100;

/// Maximum accepted raw Logpush request body size (compressed).
pub const LOGPUSH_MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

/// Maximum accepted decompressed Logpush body size. Bounds gzip expansion.
pub const LOGPUSH_MAX_DECOMPRESSED_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntakeError {
    /// Shared-secret or allowlist verification failed.
    Unauthorized(String),
    /// Payload did not match the provider's documented envelope.
    BadRequest(String),
}

impl std::fmt::Display for IntakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IntakeError::Unauthorized(message) => write!(f, "unauthorized: {message}"),
            IntakeError::BadRequest(message) => write!(f, "bad request: {message}"),
        }
    }
}

impl std::error::Error for IntakeError {}

/// Normalize an AWS SNS HTTP envelope (Notification or SubscriptionConfirmation).
pub fn normalize_sns_envelope(payload: &Value) -> Result<IncomingEvent, IntakeError> {
    let message_type = payload
        .get("Type")
        .and_then(Value::as_str)
        .ok_or_else(|| IntakeError::BadRequest("missing SNS Type field".into()))?;
    let topic_arn = string_field(payload, "TopicArn");
    let message_id = string_field(payload, "MessageId");

    match message_type {
        "Notification" => {
            let message = payload
                .get("Message")
                .and_then(Value::as_str)
                .ok_or_else(|| IntakeError::BadRequest("missing SNS Message".into()))?;
            let subject = string_field(payload, "Subject");

            if let Ok(inner) = serde_json::from_str::<Value>(message)
                && inner.get("AlarmName").and_then(Value::as_str).is_some()
            {
                return Ok(intake_event(
                    "aws.cloudwatch-alarm",
                    json!({
                        "alarm_name": inner["AlarmName"],
                        "new_state": inner["NewStateValue"],
                        "old_state": inner["OldStateValue"],
                        "reason": inner["NewStateReason"],
                        "region": inner["Region"],
                        "state_change_time": inner["StateChangeTime"],
                        "subject": subject,
                        "topic_arn": topic_arn,
                        "message_id": message_id,
                    }),
                ));
            }

            Ok(intake_event(
                "aws.sns-notification",
                json!({
                    "subject": subject,
                    "message": message,
                    "topic_arn": topic_arn,
                    "message_id": message_id,
                }),
            ))
        }
        "SubscriptionConfirmation" | "UnsubscribeConfirmation" => Ok(intake_event(
            "aws.sns-subscription-confirmation",
            json!({
                "confirmation_type": message_type,
                "topic_arn": topic_arn,
                "subscribe_url": string_field(payload, "SubscribeURL"),
                "message_id": message_id,
            }),
        )),
        other => Err(IntakeError::BadRequest(format!(
            "unsupported SNS Type '{other}'"
        ))),
    }
}

/// Returns true when `topic_arn` passes the configured allowlist.
/// An empty allowlist accepts every topic.
pub fn topic_allowed(allowlist: &[String], topic_arn: &str) -> bool {
    allowlist.is_empty() || allowlist.iter().any(|allowed| allowed == topic_arn)
}

/// Normalize an AWS EventBridge event delivered via an API destination.
pub fn normalize_eventbridge(payload: &Value) -> Result<IncomingEvent, IntakeError> {
    let source = payload
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| IntakeError::BadRequest("missing EventBridge source".into()))?;
    let detail_type = payload
        .get("detail-type")
        .and_then(Value::as_str)
        .ok_or_else(|| IntakeError::BadRequest("missing EventBridge detail-type".into()))?;

    Ok(intake_event(
        format!("aws.eventbridge.{}", slugify(detail_type)),
        json!({
            "id": string_field(payload, "id"),
            "source": source,
            "detail_type": detail_type,
            "account": string_field(payload, "account"),
            "region": string_field(payload, "region"),
            "time": string_field(payload, "time"),
            "resources": payload.get("resources").cloned().unwrap_or(Value::Null),
            "detail": payload.get("detail").cloned().unwrap_or(Value::Null),
        }),
    ))
}

/// Normalize a Cloudflare Notifications generic-webhook payload.
pub fn normalize_cf_notification(payload: &Value) -> Result<IncomingEvent, IntakeError> {
    let alert_type = payload
        .get("alert_type")
        .and_then(Value::as_str)
        .ok_or_else(|| IntakeError::BadRequest("missing Cloudflare alert_type".into()))?;

    Ok(intake_event(
        format!("cloudflare.{alert_type}"),
        json!({
            "alert_type": alert_type,
            "alert_event": string_field(payload, "alert_event"),
            "name": string_field(payload, "name"),
            "text": string_field(payload, "text"),
            "policy_id": string_field(payload, "policy_id"),
            "alert_correlation_id": string_field(payload, "alert_correlation_id"),
            "account_id": string_field(payload, "account_id"),
            "ts": payload.get("ts").cloned().unwrap_or(Value::Null),
            "data": payload.get("data").cloned().unwrap_or(Value::Null),
        }),
    ))
}

/// Verify a shared secret header in constant time.
/// A missing/empty `expected` accepts everything (endpoint opted out of auth).
pub fn verify_secret(provided: Option<&str>, expected: Option<&str>) -> Result<(), IntakeError> {
    use subtle::ConstantTimeEq;

    let Some(expected) = expected.filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    let provided =
        provided.ok_or_else(|| IntakeError::Unauthorized("missing shared secret header".into()))?;

    // Length itself is not treated as secret (standard practice); the byte compare is.
    if provided.len() != expected.len() {
        return Err(IntakeError::Unauthorized("shared secret mismatch".into()));
    }
    if provided.as_bytes().ct_eq(expected.as_bytes()).into() {
        Ok(())
    } else {
        Err(IntakeError::Unauthorized("shared secret mismatch".into()))
    }
}

/// Cache of SNS signing certificates keyed by SigningCertURL.
/// Certificates are fetched once per URL and reused.
/// Cached certificates are refetched after this many seconds.
const SNS_CERT_CACHE_TTL_SECS: u64 = 24 * 60 * 60;

#[derive(Default)]
pub struct SnsCertCache {
    inner: Mutex<HashMap<String, SnsCertEntry>>,
}

struct SnsCertEntry {
    key: RsaPublicKey,
    fetched_at: std::time::Instant,
    not_after_unix: i64,
}

impl SnsCertCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, url: &str, key: RsaPublicKey, not_after_unix: i64) {
        self.inner.lock().expect("cert cache poisoned").insert(
            url.to_string(),
            SnsCertEntry {
                key,
                fetched_at: std::time::Instant::now(),
                not_after_unix,
            },
        );
    }

    fn get(&self, url: &str) -> Option<RsaPublicKey> {
        let cache = self.inner.lock().expect("cert cache poisoned");
        let entry = cache.get(url)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs() as i64;
        (entry.fetched_at.elapsed().as_secs() < SNS_CERT_CACHE_TTL_SECS
            && now < entry.not_after_unix)
            .then(|| entry.key.clone())
    }
}

fn check_cert_validity(not_before: i64, not_after: i64, now: i64) -> Result<(), IntakeError> {
    if now < not_before {
        return Err(IntakeError::Unauthorized(
            "signing certificate is not yet valid".into(),
        ));
    }
    if now >= not_after {
        return Err(IntakeError::Unauthorized(
            "signing certificate has expired".into(),
        ));
    }
    Ok(())
}

/// Build the newline-delimited canonical string SNS signs.
/// Field order and inclusion follow the SNS documentation per message type.
pub fn sns_canonical_string(payload: &Value) -> Result<String, IntakeError> {
    let message_type = payload
        .get("Type")
        .and_then(Value::as_str)
        .ok_or_else(|| IntakeError::BadRequest("missing SNS Type field".into()))?;
    let fields: &[&str] = match message_type {
        "Notification" => &[
            "Message",
            "MessageId",
            "Subject",
            "Timestamp",
            "TopicArn",
            "Type",
        ],
        "SubscriptionConfirmation" | "UnsubscribeConfirmation" => &[
            "Message",
            "MessageId",
            "SubscribeURL",
            "Timestamp",
            "Token",
            "TopicArn",
            "Type",
        ],
        other => {
            return Err(IntakeError::BadRequest(format!(
                "unsupported SNS Type '{other}'"
            )));
        }
    };

    let mut canonical = String::new();
    for field in fields {
        if let Some(value) = payload.get(*field).and_then(Value::as_str) {
            canonical.push_str(field);
            canonical.push('\n');
            canonical.push_str(value);
            canonical.push('\n');
        }
    }
    Ok(canonical)
}

/// Validate an SNS SigningCertURL: parsed as a real URL so userinfo/port
/// tricks cannot smuggle a different host past a text check.
pub fn validate_signing_cert_url(url: &str) -> Result<(), IntakeError> {
    let parsed = reqwest::Url::parse(url).map_err(|error| {
        IntakeError::Unauthorized(format!("SigningCertURL is not a URL: {error}"))
    })?;
    let host = parsed.host_str().unwrap_or_default();
    let valid = parsed.scheme() == "https"
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && host.starts_with("sns.")
        && host.ends_with(".amazonaws.com")
        && parsed.port().is_none()
        && parsed.path().ends_with(".pem");
    if valid {
        Ok(())
    } else {
        Err(IntakeError::Unauthorized(format!(
            "SigningCertURL is not a valid SNS certificate URL: {url}"
        )))
    }
}

/// Parse a PEM X.509 certificate into an RSA public key and its expiry.
pub fn parse_signing_cert(pem: &str) -> Result<(RsaPublicKey, i64), IntakeError> {
    use rsa::pkcs8::DecodePublicKey;

    let (_, pem_block) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).map_err(|error| {
        IntakeError::Unauthorized(format!("invalid signing certificate: {error}"))
    })?;
    let cert = pem_block.parse_x509().map_err(|error| {
        IntakeError::Unauthorized(format!("invalid signing certificate: {error}"))
    })?;
    check_cert_validity(
        cert.validity().not_before.timestamp(),
        cert.validity().not_after.timestamp(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or_default(),
    )?;
    let not_after = cert.validity().not_after.timestamp();
    let key = RsaPublicKey::from_public_key_der(cert.public_key().raw).map_err(|error| {
        IntakeError::Unauthorized(format!("certificate key is not RSA: {error}"))
    })?;
    Ok((key, not_after))
}

/// Verify the SNS message signature against a known public key.
pub fn verify_sns_signature_with_key(
    payload: &Value,
    key: &RsaPublicKey,
) -> Result<(), IntakeError> {
    let canonical = sns_canonical_string(payload)?;
    let signature_b64 = payload
        .get("Signature")
        .and_then(Value::as_str)
        .ok_or_else(|| IntakeError::Unauthorized("missing SNS Signature".into()))?;
    let signature = base64::engine::general_purpose::STANDARD
        .decode(signature_b64)
        .map_err(|error| {
            IntakeError::Unauthorized(format!("invalid SNS Signature encoding: {error}"))
        })?;
    let version = payload
        .get("SignatureVersion")
        .and_then(Value::as_str)
        .unwrap_or("1");

    let verified = match version {
        "2" => {
            use sha2::Digest as _;
            let digest = sha2::Sha256::digest(canonical.as_bytes());
            key.verify(
                rsa::Pkcs1v15Sign::new::<sha2::Sha256>(),
                &digest,
                &signature,
            )
        }
        "1" => {
            use sha1::Digest as _;
            let digest = sha1::Sha1::digest(canonical.as_bytes());
            key.verify(rsa::Pkcs1v15Sign::new::<sha1::Sha1>(), &digest, &signature)
        }
        other => {
            return Err(IntakeError::Unauthorized(format!(
                "unsupported SNS SignatureVersion '{other}'"
            )));
        }
    };

    verified.map_err(|_| IntakeError::Unauthorized("SNS signature verification failed".into()))
}

/// Fetch (with cache) the SNS signing certificate and verify the message.
pub async fn verify_sns_signature(
    payload: &Value,
    cache: &SnsCertCache,
) -> Result<(), IntakeError> {
    let url = payload
        .get("SigningCertURL")
        .and_then(Value::as_str)
        .ok_or_else(|| IntakeError::Unauthorized("missing SigningCertURL".into()))?;
    validate_signing_cert_url(url)?;

    let key = match cache.get(url) {
        Some(key) => key,
        None => {
            let pem = reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|error| IntakeError::Unauthorized(format!("cert client: {error}")))?
                .get(url)
                .send()
                .await
                .map_err(|error| IntakeError::Unauthorized(format!("cert fetch failed: {error}")))?
                .text()
                .await
                .map_err(|error| IntakeError::Unauthorized(format!("cert read failed: {error}")))?;
            let (key, not_after) = parse_signing_cert(&pem)?;
            cache.insert(url, key.clone(), not_after);
            key
        }
    };

    verify_sns_signature_with_key(payload, &key)
}

/// Normalize a Cloudflare Logpush HTTP batch (NDJSON, optionally gzipped)
/// into a single batch event with a capped record list.
pub fn normalize_cf_logpush_batch(
    dataset: &str,
    body: &[u8],
    content_encoding: Option<&str>,
) -> Result<IncomingEvent, IntakeError> {
    if body.len() > LOGPUSH_MAX_BODY_BYTES {
        return Err(IntakeError::BadRequest(format!(
            "Logpush body exceeds {LOGPUSH_MAX_BODY_BYTES} bytes"
        )));
    }
    let is_gzip = content_encoding.is_some_and(|value| value.contains("gzip"))
        || body.starts_with(&[0x1f, 0x8b]);
    let decoded = if is_gzip {
        use std::io::Read;
        let mut output = Vec::new();
        flate2::read::GzDecoder::new(body)
            .take(LOGPUSH_MAX_DECOMPRESSED_BYTES + 1)
            .read_to_end(&mut output)
            .map_err(|error| IntakeError::BadRequest(format!("invalid gzip body: {error}")))?;
        if output.len() as u64 > LOGPUSH_MAX_DECOMPRESSED_BYTES {
            return Err(IntakeError::BadRequest(format!(
                "decompressed Logpush body exceeds {LOGPUSH_MAX_DECOMPRESSED_BYTES} bytes"
            )));
        }
        output
    } else {
        body.to_vec()
    };

    let text = String::from_utf8(decoded)
        .map_err(|error| IntakeError::BadRequest(format!("body is not UTF-8: {error}")))?;
    let mut records = Vec::new();
    let mut record_count = 0usize;
    for (index, line) in text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
    {
        let record: Value = serde_json::from_str(line).map_err(|error| {
            IntakeError::BadRequest(format!("invalid NDJSON record #{}: {error}", index + 1))
        })?;
        record_count += 1;
        if records.len() < LOGPUSH_RECORD_CAP {
            records.push(record);
        }
    }
    if record_count == 0 {
        return Err(IntakeError::BadRequest("empty Logpush batch".into()));
    }

    Ok(intake_event(
        format!("cloudflare.logpush.{dataset}"),
        json!({
            "dataset": dataset,
            "record_count": record_count,
            "truncated": record_count > LOGPUSH_RECORD_CAP,
            "records": records,
        }),
    ))
}

#[cfg(test)]
mod logpush_limit_tests {
    use super::*;

    #[test]
    fn cf_logpush_gzip_bomb_is_rejected() {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let chunk = vec![b'0'; 1024 * 1024];
        for _ in 0..6 {
            encoder.write_all(&chunk).unwrap();
        }
        let gzipped = encoder.finish().unwrap();
        assert!(gzipped.len() < 1024 * 1024, "zeros compress well");

        let error = normalize_cf_logpush_batch("http_requests", &gzipped, Some("gzip"))
            .expect_err("decompressed body over the cap must be rejected");
        assert!(error.to_string().contains("decompressed"));
    }

    #[test]
    fn cf_logpush_oversized_raw_body_is_rejected() {
        let body = vec![b'x'; LOGPUSH_MAX_BODY_BYTES + 1];
        assert!(normalize_cf_logpush_batch("http_requests", &body, None).is_err());
    }
}

fn intake_event(kind: impl Into<String>, payload: Value) -> IncomingEvent {
    IncomingEvent {
        kind: kind.into(),
        channel: None,
        mention: None,
        format: None,
        template: None,
        payload,
    }
}

fn string_field(payload: &Value, field: &str) -> Value {
    payload.get(field).cloned().unwrap_or(Value::Null)
}

fn slugify(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sns_alarm_envelope() -> Value {
        json!({
            "Type": "Notification",
            "MessageId": "22b80b92-idea",
            "TopicArn": "arn:aws:sns:us-east-1:123456789012:clawhip-alarms",
            "Subject": "ALARM: \"ServerCpuTooHigh\" in US East (N. Virginia)",
            "Message": "{\"AlarmName\":\"ServerCpuTooHigh\",\"NewStateValue\":\"ALARM\",\"OldStateValue\":\"OK\",\"NewStateReason\":\"Threshold Crossed\",\"StateChangeTime\":\"2026-07-22T12:00:00.000+0000\",\"Region\":\"US East (N. Virginia)\",\"AlarmArn\":\"arn:aws:cloudwatch:us-east-1:123456789012:alarm:ServerCpuTooHigh\",\"Trigger\":{\"MetricName\":\"CPUUtilization\"}}",
            "Timestamp": "2026-07-22T12:00:01.000Z"
        })
    }

    #[test]
    fn sns_cloudwatch_alarm_maps_to_typed_event() {
        let event = normalize_sns_envelope(&sns_alarm_envelope()).unwrap();

        assert_eq!(event.kind, "aws.cloudwatch-alarm");
        assert_eq!(event.payload["alarm_name"], "ServerCpuTooHigh");
        assert_eq!(event.payload["new_state"], "ALARM");
        assert_eq!(event.payload["old_state"], "OK");
        assert_eq!(
            event.payload["topic_arn"],
            "arn:aws:sns:us-east-1:123456789012:clawhip-alarms"
        );
        assert_eq!(event.payload["message_id"], "22b80b92-idea");
    }

    #[test]
    fn sns_plain_notification_keeps_subject_and_message() {
        let payload = json!({
            "Type": "Notification",
            "MessageId": "abc",
            "TopicArn": "arn:aws:sns:us-east-1:123456789012:budgets",
            "Subject": "Budget exceeded",
            "Message": "your budget is 85% spent",
            "Timestamp": "2026-07-22T12:00:01.000Z"
        });

        let event = normalize_sns_envelope(&payload).unwrap();

        assert_eq!(event.kind, "aws.sns-notification");
        assert_eq!(event.payload["subject"], "Budget exceeded");
        assert_eq!(event.payload["message"], "your budget is 85% spent");
    }

    #[test]
    fn sns_subscription_confirmation_maps_for_operator() {
        let payload = json!({
            "Type": "SubscriptionConfirmation",
            "MessageId": "165545c9",
            "Token": "2336412f37",
            "TopicArn": "arn:aws:sns:us-east-1:123456789012:clawhip-alarms",
            "Message": "You have chosen to subscribe ...",
            "SubscribeURL": "https://sns.us-east-1.amazonaws.com/?Action=ConfirmSubscription&Token=2336412f37",
            "Timestamp": "2026-07-22T12:00:01.000Z"
        });

        let event = normalize_sns_envelope(&payload).unwrap();

        assert_eq!(event.kind, "aws.sns-subscription-confirmation");
        assert_eq!(
            event.payload["subscribe_url"],
            "https://sns.us-east-1.amazonaws.com/?Action=ConfirmSubscription&Token=2336412f37"
        );
    }

    #[test]
    fn sns_unknown_type_rejected() {
        let forged = json!({"Type": "Forged", "MessageId": "x"});
        assert!(normalize_sns_envelope(&forged).is_err());

        let unsubscribe = json!({
            "Type": "UnsubscribeConfirmation",
            "MessageId": "x",
            "TopicArn": "arn:aws:sns:us-east-1:123456789012:clawhip-alarms",
            "SubscribeURL": "https://sns.us-east-1.amazonaws.com/?Action=ConfirmSubscription&Token=abc"
        });
        let event = normalize_sns_envelope(&unsubscribe).unwrap();
        assert_eq!(event.kind, "aws.sns-subscription-confirmation");
    }

    #[test]
    fn topic_allowlist_enforced() {
        let allowlist = vec!["arn:aws:sns:us-east-1:123456789012:clawhip-alarms".to_string()];

        assert!(topic_allowed(
            &allowlist,
            "arn:aws:sns:us-east-1:123456789012:clawhip-alarms"
        ));
        assert!(!topic_allowed(
            &allowlist,
            "arn:aws:sns:us-east-1:999999999999:evil"
        ));
        assert!(topic_allowed(
            &[],
            "arn:aws:sns:us-east-1:999999999999:anything"
        ));
    }

    #[test]
    fn eventbridge_maps_detail_type_slug_and_preserves_detail() {
        let payload = json!({
            "version": "0",
            "id": "6a7e8feb-b491",
            "detail-type": "GuardDuty Finding",
            "source": "aws.guardduty",
            "account": "111122223333",
            "time": "2026-07-22T18:43:48Z",
            "region": "us-west-1",
            "resources": ["arn:aws:guardduty:us-west-1:111122223333:detector/abc/finding/def"],
            "detail": {"id": "finding-id", "severity": 8.0, "type": "Trojan:EC2/DNSDataExfiltration"}
        });

        let event = normalize_eventbridge(&payload).unwrap();

        assert_eq!(event.kind, "aws.eventbridge.guardduty-finding");
        assert_eq!(event.payload["source"], "aws.guardduty");
        assert_eq!(event.payload["detail_type"], "GuardDuty Finding");
        assert_eq!(event.payload["detail"]["severity"], 8.0);
        assert_eq!(event.payload["id"], "6a7e8feb-b491");
    }

    #[test]
    fn eventbridge_requires_source_and_detail_type() {
        assert!(normalize_eventbridge(&json!({"source": "aws.ec2"})).is_err());
        assert!(normalize_eventbridge(&json!({"detail-type": "X"})).is_err());
    }

    #[test]
    fn cf_notification_maps_alert_type_and_preserves_data() {
        let payload = json!({
            "name": "Origin health check",
            "text": "Health check origin-api is unhealthy",
            "data": {
                "health_check_name": "origin-api",
                "new_health_status": "unhealthy",
                "reason": "TCP connection failed"
            },
            "ts": 1753200000,
            "account_id": "9035f53656c247e895c5a6939ae8a0e0",
            "policy_id": "749b911ea5d04344a58e45edd099b328",
            "alert_type": "health_check_status_notification",
            "alert_correlation_id": "000eaa907ed24e78946d3a93adb2ae57",
            "alert_event": "ALERT_STATE_EVENT_START"
        });

        let event = normalize_cf_notification(&payload).unwrap();

        assert_eq!(event.kind, "cloudflare.health_check_status_notification");
        assert_eq!(event.payload["alert_event"], "ALERT_STATE_EVENT_START");
        assert_eq!(
            event.payload["alert_correlation_id"],
            "000eaa907ed24e78946d3a93adb2ae57"
        );
        assert_eq!(event.payload["data"]["new_health_status"], "unhealthy");
        assert_eq!(
            event.payload["text"],
            "Health check origin-api is unhealthy"
        );
    }

    #[test]
    fn cf_notification_requires_alert_type() {
        assert!(normalize_cf_notification(&json!({"text": "hi"})).is_err());
    }

    #[test]
    fn secret_verification_constant_time_semantics() {
        assert!(verify_secret(Some("s3cret"), Some("s3cret")).is_ok());
        assert!(verify_secret(Some("wrong"), Some("s3cret")).is_err());
        assert!(verify_secret(None, Some("s3cret")).is_err());
        // Endpoint opted out of auth: no configured secret accepts anything.
        assert!(verify_secret(None, None).is_ok());
        assert!(verify_secret(Some("anything"), Some("")).is_ok());
    }

    #[test]
    fn cf_logpush_ndjson_batch_splits_records_and_caps() {
        let mut lines = Vec::new();
        for i in 0..120 {
            lines.push(format!(
                "{{\"RayID\":\"ray-{i}\",\"EdgeResponseStatus\":403}}"
            ));
        }
        let body = lines.join("\n");

        let event = normalize_cf_logpush_batch("firewall_events", body.as_bytes(), None).unwrap();

        assert_eq!(event.kind, "cloudflare.logpush.firewall_events");
        assert_eq!(event.payload["record_count"], 120);
        assert_eq!(
            event.payload["records"].as_array().unwrap().len(),
            LOGPUSH_RECORD_CAP
        );
        assert_eq!(event.payload["truncated"], true);
    }

    #[test]
    fn cf_logpush_gzip_body_decodes() {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(b"{\"RayID\":\"ray-1\"}\n{\"RayID\":\"ray-2\"}\n")
            .unwrap();
        let gzipped = encoder.finish().unwrap();

        let event = normalize_cf_logpush_batch("audit_logs_v2", &gzipped, Some("gzip")).unwrap();

        assert_eq!(event.payload["record_count"], 2);
        assert_eq!(event.payload["records"][0]["RayID"], "ray-1");
    }
}

#[cfg(test)]
mod sns_signature_tests {
    use super::*;
    use serde_json::json;

    const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sns");
    const CERT_URL: &str = "https://sns.us-east-1.amazonaws.com/SimpleNotificationService-test.pem";

    fn fixture_key() -> rsa::RsaPublicKey {
        let pem = std::fs::read_to_string(format!("{FIXTURE_DIR}/cert.pem")).unwrap();
        parse_signing_cert(&pem).unwrap().0
    }

    fn signed_payload() -> Value {
        json!({
            "Type": "Notification",
            "MessageId": "22b80b92",
            "TopicArn": "arn:aws:sns:us-east-1:123456789012:clawhip-alarms",
            "Subject": "ALARM: ServerCpuTooHigh",
            "Message": "{\"AlarmName\":\"ServerCpuTooHigh\",\"NewStateValue\":\"ALARM\"}",
            "Timestamp": "2026-07-22T12:00:01.000Z",
            "SignatureVersion": "2",
            "Signature": std::fs::read_to_string(format!("{FIXTURE_DIR}/sig.b64")).unwrap().trim(),
            "SigningCertURL": CERT_URL
        })
    }

    #[test]
    fn canonical_string_matches_openssl_signed_fixture() {
        let expected = std::fs::read_to_string(format!("{FIXTURE_DIR}/canonical.txt")).unwrap();
        assert_eq!(sns_canonical_string(&signed_payload()).unwrap(), expected);
    }

    #[test]
    fn signing_cert_url_validation() {
        assert!(validate_signing_cert_url(CERT_URL).is_ok());
        assert!(validate_signing_cert_url("http://sns.us-east-1.amazonaws.com/x.pem").is_err());
        assert!(validate_signing_cert_url("https://evil.example.com/x.pem").is_err());
        assert!(
            validate_signing_cert_url("https://sns.evil.amazonaws.com.evil.com/x.pem").is_err()
        );
        assert!(validate_signing_cert_url("https://sns.us-east-1.amazonaws.com/x.exe").is_err());
    }

    #[test]
    fn signing_cert_url_rejects_authority_smuggling() {
        assert!(
            validate_signing_cert_url(
                "https://sns.us-east-1.amazonaws.com:443@evil.example/attacker.pem"
            )
            .is_err()
        );
        assert!(
            validate_signing_cert_url("https://sns.us-east-1.amazonaws.com:444/x.pem").is_err()
        );
        assert!(
            validate_signing_cert_url("https://user@sns.us-east-1.amazonaws.com/x.pem").is_err()
        );
    }

    #[test]
    fn cert_validity_window_enforced() {
        assert!(check_cert_validity(100, 200, 150).is_ok());
        assert!(check_cert_validity(100, 200, 50).is_err());
        assert!(check_cert_validity(100, 200, 250).is_err());
        assert!(check_cert_validity(100, 200, 200).is_err());
    }

    #[test]
    fn valid_signature_accepted() {
        let payload = signed_payload();
        let key = fixture_key();
        verify_sns_signature_with_key(&payload, &key).expect("valid signature must verify");
    }

    #[test]
    fn tampered_message_rejected() {
        let mut payload = signed_payload();
        payload["Message"] = json!("{\"AlarmName\":\"Forged\"}");
        let key = fixture_key();
        assert!(verify_sns_signature_with_key(&payload, &key).is_err());
    }

    #[test]
    fn tampered_signature_rejected() {
        let mut payload = signed_payload();
        let mut sig = payload["Signature"].as_str().unwrap().to_string();
        sig.replace_range(0..4, "AAAA");
        payload["Signature"] = json!(sig);
        let key = fixture_key();
        assert!(verify_sns_signature_with_key(&payload, &key).is_err());
    }

    #[test]
    fn cache_rejects_entries_past_certificate_expiry() {
        let cache = SnsCertCache::new();
        let past = 1_000_000_000; // 2001-09-09
        cache.insert(CERT_URL, fixture_key(), past);
        assert!(
            cache.get(CERT_URL).is_none(),
            "expired certificate must not be served from cache"
        );
    }

    #[tokio::test]
    async fn verify_sns_signature_uses_prepopulated_cache_without_network() {
        let cache = SnsCertCache::new();
        let far_future = 4_102_444_800; // 2100-01-01
        cache.insert(CERT_URL, fixture_key(), far_future);
        verify_sns_signature(&signed_payload(), &cache)
            .await
            .expect("cached cert should verify without network");
    }
}
