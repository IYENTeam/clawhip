use async_trait::async_trait;
use reqwest::Client;
use serde_json::{Value, json};
use std::time::Duration;

use crate::Result;

use super::{Sink, SinkMessage, SinkTarget};

#[derive(Clone)]
pub struct IyenSystemSink {
    client: Client,
    url: String,
    auth_token: String,
}

impl IyenSystemSink {
    pub fn new(url: String, auth_token: String) -> Self {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            client,
            url,
            auth_token,
        }
    }

    pub fn is_configured(url: &Option<String>, auth_token: &Option<String>) -> bool {
        url.as_ref().map(|u| !u.trim().is_empty()).unwrap_or(false)
            && auth_token
                .as_ref()
                .map(|t| !t.trim().is_empty())
                .unwrap_or(false)
    }

    fn endpoint(&self) -> String {
        format!("{}/event", self.url.trim_end_matches('/'))
    }
}

fn extract_str<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(|v| v.as_str())
}

fn extract_u64(payload: &Value, key: &str) -> Option<u64> {
    payload.get(key).and_then(|v| v.as_u64())
}

fn derive_action(event_kind: &str, payload: &Value) -> String {
    if let Some(action) = extract_str(payload, "action") {
        return action.to_string();
    }
    match event_kind.rsplit_once('.') {
        Some((_, suffix)) => suffix.replace('-', "_"),
        None => event_kind.replace('-', "_"),
    }
}

fn build_iyensystem_body(message: &SinkMessage) -> Value {
    let repo = extract_str(&message.payload, "repo")
        .unwrap_or("")
        .to_string();
    let number = extract_u64(&message.payload, "number").unwrap_or(0);
    let action = derive_action(&message.event_kind, &message.payload);

    json!({
        "event_type": message.event_kind,
        "repo": repo,
        "number": number,
        "action": action,
        "payload": message.payload,
    })
}

#[async_trait]
impl Sink for IyenSystemSink {
    async fn send(&self, _target: &SinkTarget, message: &SinkMessage) -> Result<()> {
        let url = self.endpoint();
        let body = build_iyensystem_body(message);

        eprintln!(
            "clawhip iyensystem sink: event_kind={} url={}",
            message.event_kind, url
        );

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.auth_token))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("IyenSystem request to {url} failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read body>".to_string());
            return Err(format!("IyenSystem POST /event failed: {status} — {body}").into());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::MessageFormat;

    fn message(kind: &str, payload: Value) -> SinkMessage {
        SinkMessage {
            event_kind: kind.into(),
            format: MessageFormat::Compact,
            content: "rendered".into(),
            payload,
        }
    }

    #[test]
    fn is_configured_requires_both_url_and_token() {
        assert!(!IyenSystemSink::is_configured(&None, &None));
        assert!(!IyenSystemSink::is_configured(
            &Some("http://localhost".into()),
            &None
        ));
        assert!(!IyenSystemSink::is_configured(&None, &Some("token".into())));
        assert!(IyenSystemSink::is_configured(
            &Some("http://localhost".into()),
            &Some("token".into())
        ));
        assert!(!IyenSystemSink::is_configured(
            &Some("   ".into()),
            &Some("token".into())
        ));
        assert!(!IyenSystemSink::is_configured(
            &Some("http://localhost".into()),
            &Some("".into())
        ));
    }

    #[test]
    fn endpoint_strips_trailing_slash() {
        let sink = IyenSystemSink::new("http://127.0.0.1:25295/".into(), "tok".into());
        assert_eq!(sink.endpoint(), "http://127.0.0.1:25295/event");
        let sink2 = IyenSystemSink::new("http://127.0.0.1:25295".into(), "tok".into());
        assert_eq!(sink2.endpoint(), "http://127.0.0.1:25295/event");
    }

    #[test]
    fn body_carries_event_type_repo_number_action_and_full_payload() {
        let payload = json!({
            "repo": "Org/Repo",
            "number": 42,
            "title": "broken",
            "sender": {"login": "openclaw-bot"},
        });
        let msg = message("github.issue-opened", payload.clone());
        let body = build_iyensystem_body(&msg);

        assert_eq!(body["event_type"], "github.issue-opened");
        assert_eq!(body["repo"], "Org/Repo");
        assert_eq!(body["number"], 42);
        assert_eq!(body["action"], "issue_opened");
        assert_eq!(body["payload"], payload);
    }

    #[test]
    fn explicit_action_in_payload_overrides_derived_action() {
        let payload = json!({
            "repo": "Org/Repo",
            "number": 7,
            "action": "labeled",
            "label": {"name": "iyen:auto-fix"},
        });
        let msg = message("github.issues-labeled", payload);
        let body = build_iyensystem_body(&msg);
        assert_eq!(body["action"], "labeled");
    }

    #[test]
    fn missing_repo_and_number_default_to_safe_values() {
        let payload = json!({});
        let msg = message("custom.heartbeat", payload);
        let body = build_iyensystem_body(&msg);
        assert_eq!(body["repo"], "");
        assert_eq!(body["number"], 0);
        assert_eq!(body["action"], "heartbeat");
    }
}
