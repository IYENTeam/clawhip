use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, RETRY_AFTER};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

use crate::Result;
use crate::config::AppConfig;
use crate::events::MessageFormat;
use crate::sink::{SinkMessage, SinkTarget};

const MAX_ATTEMPTS: u32 = 3;

#[derive(Clone)]
pub struct SlackClient {
    webhook_client: reqwest::Client,
    bot_client: Option<reqwest::Client>,
    api_base: String,
}

impl SlackClient {
    pub fn new() -> Self {
        Self {
            webhook_client: reqwest::Client::new(),
            bot_client: None,
            api_base: default_api_base(),
        }
    }

    pub fn from_config(config: Arc<AppConfig>) -> Result<Self> {
        let bot_client = if let Some(token) = config.effective_slack_token() {
            let mut headers = HeaderMap::new();
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

            Some(
                reqwest::Client::builder()
                    .default_headers(headers)
                    .build()?,
            )
        } else {
            None
        };

        Ok(Self {
            webhook_client: reqwest::Client::new(),
            bot_client,
            api_base: std::env::var("OP_PI_SLACK_API_BASE").unwrap_or_else(|_| default_api_base()),
        })
    }

    pub async fn send(&self, target: &SinkTarget, message: &SinkMessage) -> Result<()> {
        match target {
            SinkTarget::SlackChannel(channel) => self.send_channel(channel, message).await,
            SinkTarget::SlackWebhook(webhook_url) => self.send_webhook(webhook_url, message).await,
            SinkTarget::DiscordChannel(_)
            | SinkTarget::DiscordThread(_)
            | SinkTarget::DiscordWebhook(_) => {
                Err("cannot send Discord target via Slack client".into())
            }
            SinkTarget::LocalFile(_) => Err("cannot send localfile target via Slack client".into()),
        }
    }

    pub async fn send_channel(&self, channel: &str, message: &SinkMessage) -> Result<()> {
        let client = self.bot_client.as_ref().ok_or(
            "Slack channel delivery requires a bot token; configure [providers.slack].token or OP_PI_SLACK_BOT_TOKEN",
        )?;
        let url = format!("{}/chat.postMessage", self.api_base);

        let mut attempt = 0;
        loop {
            attempt += 1;
            let response = client
                .post(&url)
                .json(&channel_payload(channel, message))
                .send()
                .await?;
            let status = response.status();

            if status == StatusCode::TOO_MANY_REQUESTS && attempt < MAX_ATTEMPTS {
                let retry_after_secs = response
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(1);
                tokio::time::sleep(Duration::from_secs(retry_after_secs)).await;
                continue;
            }

            let body: Value = response.json().await.unwrap_or_else(|_| json!({}));
            // Slack reports API errors with HTTP 200 + {"ok": false, "error": "..."}.
            if status.is_success() && body.get("ok").and_then(Value::as_bool) == Some(true) {
                return Ok(());
            }

            let error = body
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown_error");
            if status.is_server_error() && attempt < MAX_ATTEMPTS {
                tokio::time::sleep(Duration::from_millis(100 * u64::from(attempt))).await;
                continue;
            }
            return Err(format!("Slack chat.postMessage failed with {status}: {error}").into());
        }
    }

    pub async fn send_webhook(&self, webhook_url: &str, message: &SinkMessage) -> Result<()> {
        let response = self
            .webhook_client
            .post(webhook_url)
            .json(&webhook_payload(message))
            .send()
            .await?;

        if response.status().is_success() {
            return Ok(());
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Err(format!("Slack webhook request failed with {status}: {body}").into())
    }
}

impl Default for SlackClient {
    fn default() -> Self {
        Self::new()
    }
}

fn default_api_base() -> String {
    "https://slack.com/api".to_string()
}

fn channel_payload(channel: &str, message: &SinkMessage) -> Value {
    let mut payload = webhook_payload(message);
    payload["channel"] = json!(channel);
    payload
}

fn webhook_payload(message: &SinkMessage) -> Value {
    let mut payload = json!({
        "text": message.content,
    });

    if matches!(
        message.format,
        MessageFormat::Compact | MessageFormat::Alert
    ) {
        payload["blocks"] = json!(slack_blocks(message));
    }

    payload
}

fn slack_blocks(message: &SinkMessage) -> Vec<Value> {
    let label = match message.format {
        MessageFormat::Alert => ":rotating_light: *Alert*",
        _ => ":speech_balloon: *Notification*",
    };

    vec![
        json!({
            "type": "section",
            "text": {
                "type": "mrkdwn",
                "text": label,
            }
        }),
        json!({
            "type": "section",
            "text": {
                "type": "mrkdwn",
                "text": message.content,
            }
        }),
        json!({
            "type": "context",
            "elements": [
                {
                    "type": "mrkdwn",
                    "text": format!("event `{}` · format `{}`", message.event_kind, message.format.as_str()),
                }
            ]
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn compact_message() -> SinkMessage {
        SinkMessage {
            event_kind: "tmux.keyword".into(),
            format: MessageFormat::Compact,
            content: "tmux:ops matched 'error' => boom".into(),
            payload: serde_json::json!({}),
            telemetry: None,
        }
    }

    fn test_client(token: &str, api_base: String) -> SlackClient {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        SlackClient {
            webhook_client: reqwest::Client::new(),
            bot_client: Some(
                reqwest::Client::builder()
                    .default_headers(headers)
                    .build()
                    .unwrap(),
            ),
            api_base,
        }
    }

    /// One-shot mock Slack API: serves each canned response once, records request bodies.
    async fn spawn_mock_slack(
        responses: Vec<String>,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        tokio::spawn(async move {
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 65536];
                let mut request = Vec::new();
                let content_length = loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    request.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&request);
                    if let Some(head_end) = text.find("\r\n\r\n") {
                        let length = text[..head_end]
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if request.len() >= head_end + 4 + length {
                            break length;
                        }
                    }
                };
                let _ = content_length;
                tx.send(String::from_utf8_lossy(&request).to_string())
                    .unwrap();
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (format!("http://{addr}"), rx)
    }

    fn http_response(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn channel_payload_includes_channel_text_and_blocks() {
        let payload = channel_payload("C123OPS", &compact_message());

        assert_eq!(
            payload.get("channel").and_then(Value::as_str),
            Some("C123OPS")
        );
        assert_eq!(
            payload.get("text").and_then(Value::as_str),
            Some("tmux:ops matched 'error' => boom")
        );
        assert!(payload.get("blocks").and_then(Value::as_array).is_some());
    }

    #[tokio::test]
    async fn send_channel_requires_bot_token() {
        let client = SlackClient::new();
        let error = client
            .send_channel("C123OPS", &compact_message())
            .await
            .expect_err("channel delivery without a bot token must fail");
        assert!(error.to_string().contains("bot token"));
    }

    #[tokio::test]
    async fn send_channel_posts_to_chat_post_message() {
        let (base, rx) = spawn_mock_slack(vec![http_response("200 OK", "{\"ok\":true}")]).await;
        let client = test_client("xoxb-test", base);

        client
            .send_channel("C123OPS", &compact_message())
            .await
            .expect("chat.postMessage should succeed");

        let request = rx.recv().unwrap();
        assert!(request.starts_with("POST /chat.postMessage"));
        assert!(request.contains("authorization: Bearer xoxb-test"));
        assert!(request.contains("\"channel\":\"C123OPS\""));
    }

    #[tokio::test]
    async fn send_channel_retries_after_429_then_succeeds() {
        let (base, rx) = spawn_mock_slack(vec![
            format!("HTTP/1.1 429 Too Many Requests\r\nretry-after: 0\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{{}}"),
            http_response("200 OK", "{\"ok\":true}"),
        ])
        .await;
        let client = test_client("xoxb-test", base);

        client
            .send_channel("C123OPS", &compact_message())
            .await
            .expect("429 then 200 should succeed via retry");

        rx.recv().unwrap();
        rx.recv().unwrap();
        assert!(rx.try_recv().is_err(), "exactly two requests expected");
    }

    #[tokio::test]
    async fn send_channel_surfaces_slack_error_envelope() {
        let (base, _rx) = spawn_mock_slack(vec![http_response(
            "200 OK",
            "{\"ok\":false,\"error\":\"channel_not_found\"}",
        )])
        .await;
        let client = test_client("xoxb-test", base);

        let error = client
            .send_channel("C_MISSING", &compact_message())
            .await
            .expect_err("ok:false envelope must be an error");
        assert!(error.to_string().contains("channel_not_found"));
    }

    #[test]
    fn compact_payload_includes_block_kit_sections() {
        let payload = webhook_payload(&SinkMessage {
            event_kind: "tmux.keyword".into(),
            format: MessageFormat::Compact,
            content: "tmux:ops matched 'error' => boom".into(),
            payload: serde_json::json!({}),
            telemetry: None,
        });

        assert_eq!(
            payload.get("text").and_then(Value::as_str),
            Some("tmux:ops matched 'error' => boom")
        );
        let blocks = payload
            .get("blocks")
            .and_then(Value::as_array)
            .expect("blocks");
        assert_eq!(blocks.len(), 3);
        assert_eq!(
            blocks[0]["text"]["text"].as_str(),
            Some(":speech_balloon: *Notification*")
        );
    }

    #[test]
    fn alert_payload_uses_alert_label() {
        let payload = webhook_payload(&SinkMessage {
            event_kind: "github.ci-failed".into(),
            format: MessageFormat::Alert,
            content: "🚨 deploy <failed> & paging".into(),
            payload: serde_json::json!({}),
            telemetry: None,
        });

        let blocks = payload
            .get("blocks")
            .and_then(Value::as_array)
            .expect("blocks");
        assert_eq!(
            blocks[0]["text"]["text"].as_str(),
            Some(":rotating_light: *Alert*")
        );
        assert_eq!(
            blocks[1]["text"]["text"].as_str(),
            Some("🚨 deploy <failed> & paging")
        );
    }
}
