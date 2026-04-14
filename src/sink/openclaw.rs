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

#[async_trait]
impl Sink for OpenClawSink {
    async fn send(&self, _target: &SinkTarget, message: &SinkMessage) -> Result<()> {
        // Use OpenClaw cron wake API to inject a system event into the main session
        let text = format!(
            "[clawhip:{}] {}\n\nPayload: {}",
            message.event_kind,
            message.content,
            serde_json::to_string_pretty(&message.payload).unwrap_or_default()
        );

        let url = format!("{}/api/cron/wake", self.gateway_url.trim_end_matches('/'));

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.gateway_token))
            .json(&json!({
                "text": text,
                "mode": "now"
            }))
            .send()
            .await
            .map_err(|e| format!("OpenClaw wake request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read body>".to_string());
            return Err(format!("OpenClaw wake failed: {status} — {body}").into());
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
        assert!(!OpenClawSink::is_configured(
            &None,
            &Some("token".into())
        ));
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
