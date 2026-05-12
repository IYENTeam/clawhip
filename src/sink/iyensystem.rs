use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;

use super::{Sink, SinkMessage, SinkTarget};
use crate::Result;

/// Sink that forwards events to the IYENSystem webhook endpoint.
/// Events are POSTed as JSON to `{base_url}/webhook/github` with HMAC auth,
/// or to `{base_url}/event` as a simple JSON event.
pub struct IyenSystemSink {
    client: Client,
    base_url: String,
    auth_token: Option<String>,
}

impl IyenSystemSink {
    pub fn new(base_url: String, auth_token: Option<String>) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            auth_token,
        }
    }
}

#[async_trait]
impl Sink for IyenSystemSink {
    async fn send(&self, _target: &SinkTarget, message: &SinkMessage) -> Result<()> {
        let url = format!("{}/event", self.base_url);
        let body = json!({
            "event_type": message.event_kind,
            "payload": message.payload,
        });

        eprintln!(
            "clawhip iyensystem sink: event_kind={} url={}",
            message.event_kind, url
        );

        let mut req = self.client.post(&url).json(&body);
        if let Some(token) = &self.auth_token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }

        let resp = req.send().await.map_err(|e| {
            anyhow::anyhow!("iyensystem sink request failed: {e}")
        })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text: String = resp.text().await.unwrap_or_default();
            eprintln!(
                "clawhip iyensystem sink: POST {} returned {} — {}",
                url, status, body_text
            );
        }

        Ok(())
    }
}
