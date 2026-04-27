use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use std::time::Duration;

use crate::Result;

use super::{Sink, SinkMessage, SinkTarget};

#[derive(Clone)]
pub struct OpenClawSink {
    client: Client,
    gateway_url: String,
    gateway_token: String,
}

impl OpenClawSink {
    pub fn new(gateway_url: String, gateway_token: String) -> Self {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            client,
            gateway_url,
            gateway_token,
        }
    }

    pub fn is_configured(gateway_url: &Option<String>, gateway_token: &Option<String>) -> bool {
        gateway_url
            .as_ref()
            .map(|u| !u.trim().is_empty())
            .unwrap_or(false)
            && gateway_token
                .as_ref()
                .map(|t| !t.trim().is_empty())
                .unwrap_or(false)
    }
}

impl OpenClawSink {
    /// Determine the hooks path based on event kind.
    /// PR events go to /hooks/pr-review (agent action),
    /// everything else goes to /hooks/wake (wake action).
    fn hooks_path_for_event(event_kind: &str, payload: &serde_json::Value) -> &'static str {
        // Check direct event kind
        if event_kind.contains("pr-status-changed") {
            return "/hooks/pr-review";
        }
        if event_kind.contains("issue-opened") {
            return "/hooks/issue-triage";
        }
        // Check batched event_kinds array
        if let Some(kinds) = payload.get("event_kinds").and_then(|v| v.as_array()) {
            for kind in kinds {
                if let Some(s) = kind.as_str() {
                    if s.contains("pr-status-changed") {
                        return "/hooks/pr-review";
                    }
                    if s.contains("issue-opened") {
                        return "/hooks/issue-triage";
                    }
                }
            }
        }
        "/hooks/wake"
    }
}

#[async_trait]
impl Sink for OpenClawSink {
    async fn send(&self, _target: &SinkTarget, message: &SinkMessage) -> Result<()> {
        let hooks_path = Self::hooks_path_for_event(&message.event_kind, &message.payload);

        let url = format!("{}{}", self.gateway_url.trim_end_matches('/'), hooks_path);

        let body = if hooks_path == "/hooks/pr-review" {
            // Send structured JSON so messageTemplate can use {{repo}}, {{number}}, etc.
            let mut pr_body = json!({
                "text": message.content,
                "content": message.content,
                "mode": "now"
            });
            // Copy payload fields (repo, number, title, etc.) to top level
            if let Some(obj) = message.payload.as_object() {
                for (k, v) in obj {
                    pr_body[k] = v.clone();
                }
            }
            // Also check batched payloads for event_kinds
            if let Some(kinds) = message
                .payload
                .get("event_kinds")
                .and_then(|v| v.as_array())
            {
                pr_body["event_kinds"] = json!(kinds);
            }
            pr_body
        } else {
            json!({
                "text": format!(
                    "[clawhip:{}] {}\n\nPayload: {}",
                    message.event_kind,
                    message.content,
                    serde_json::to_string_pretty(&message.payload).unwrap_or_default()
                ),
                "mode": "now"
            })
        };

        // Log which hooks path is being used
        eprintln!(
            "clawhip openclaw sink: event_kind={} hooks_path={} url={}",
            message.event_kind, hooks_path, url
        );

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.gateway_token))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("OpenClaw request to {hooks_path} failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read body>".to_string());
            return Err(format!("OpenClaw {hooks_path} failed: {status} — {body}").into());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_configured_requires_both_url_and_token() {
        assert!(!OpenClawSink::is_configured(&None, &None));
        assert!(!OpenClawSink::is_configured(
            &Some("http://localhost".into()),
            &None
        ));
        assert!(!OpenClawSink::is_configured(&None, &Some("token".into())));
        assert!(OpenClawSink::is_configured(
            &Some("http://localhost".into()),
            &Some("token".into())
        ));
        assert!(!OpenClawSink::is_configured(
            &Some("".into()),
            &Some("token".into())
        ));
    }
}
