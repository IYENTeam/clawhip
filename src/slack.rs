use serde_json::{Value, json};

use crate::Result;
use crate::events::MessageFormat;
use crate::sink::{SinkMessage, SinkTarget};

#[derive(Clone)]
pub struct SlackClient {
    webhook_client: reqwest::Client,
}

impl SlackClient {
    pub fn new() -> Self {
        Self {
            webhook_client: reqwest::Client::new(),
        }
    }

    pub async fn send(&self, target: &SinkTarget, message: &SinkMessage) -> Result<()> {
        match target {
            SinkTarget::SlackWebhook(webhook_url) => self.send_webhook(webhook_url, message).await,
            SinkTarget::DiscordChannel(_) | SinkTarget::DiscordWebhook(_) => {
                Err("cannot send Discord target via Slack client".into())
            }
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
    let label = event_label(message);
    let mut blocks = vec![
        json!({
            "type": "section",
            "text": {
                "type": "mrkdwn",
                "text": label,
            }
        }),
        json!({"type": "divider"}),
        json!({
            "type": "section",
            "text": {
                "type": "mrkdwn",
                "text": message.content,
            }
        }),
    ];

    if let Some(fields) = payload_fields(&message.payload) {
        blocks.push(json!({"type": "divider"}));
        blocks.push(json!({
            "type": "section",
            "fields": fields,
        }));
    }

    blocks.push(context_block(message));
    blocks
}

fn event_label(message: &SinkMessage) -> String {
    match message.event_kind.as_str() {
        k if k.contains("failed") || k.contains("error") => {
            ":x: *Failed*".to_string()
        }
        k if k.contains("blocked") || k.contains("stale") => {
            ":warning: *Attention*".to_string()
        }
        k if k.contains("started") || k.contains("created") => {
            ":arrow_forward: *Started*".to_string()
        }
        k if k.contains("finished") || k.contains("passed") || k.contains("closed") => {
            ":white_check_mark: *Completed*".to_string()
        }
        _ => match message.format {
            MessageFormat::Alert => ":rotating_light: *Alert*".to_string(),
            _ => ":speech_balloon: *Notification*".to_string(),
        },
    }
}

fn payload_fields(payload: &Value) -> Option<Vec<Value>> {
    let obj = payload.as_object()?;
    let relevant: Vec<(String, &Value)> = obj
        .iter()
        .filter(|(k, v)| {
            !k.starts_with('_')
                && *k != "mention"
                && *k != "repo_path"
                && *k != "worktree_path"
                && *k != "session_id"
                && v.is_string()
                && v.as_str().map_or(false, |s| !s.is_empty())
        })
        .map(|(k, v)| (k.clone(), v))
        .collect();

    if relevant.is_empty() {
        return None;
    }

    let pairs: Vec<Value> = relevant
        .chunks(2)
        .flat_map(|chunk| {
            chunk.iter().map(|(k, v)| {
                let display = value_display(v);
                json!({
                    "type": "mrkdwn",
                    "text": format!("*{}:*\n{}", k, display),
                })
            })
        })
        .collect();

    if pairs.is_empty() {
        None
    } else {
        Some(pairs)
    }
}

fn value_display(value: &Value) -> String {
    match value {
        Value::String(s) => s.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => value.to_string(),
    }
}

fn context_block(message: &SinkMessage) -> Value {
    let mut elements = vec![json!({
        "type": "mrkdwn",
        "text": format!("event `{}` · `{}`", message.event_kind, message.format.as_str()),
    })];

    if let Some(ref telemetry) = message.telemetry {
        if let Some(ref batch_count) = telemetry.batch_count {
            elements.push(json!({
                "type": "mrkdwn",
                "text": format!("batch `{}`", batch_count),
            }));
        }
    }

    json!({
        "type": "context",
        "elements": elements,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(blocks.len(), 4);
        assert_eq!(
            blocks[0]["text"]["text"].as_str(),
            Some(":speech_balloon: *Notification*")
        );
        assert_eq!(blocks[1]["type"].as_str(), Some("divider"));
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
            Some(":x: *Failed*")
        );
        assert_eq!(blocks[1]["type"].as_str(), Some("divider"));
        assert_eq!(
            blocks[2]["text"]["text"].as_str(),
            Some("🚨 deploy <failed> & paging")
        );
    }

    #[test]
    fn payload_fields_appear_as_block_kit_fields() {
        let payload = webhook_payload(&SinkMessage {
            event_kind: "git.commit".into(),
            format: MessageFormat::Compact,
            content: "pushed to main".into(),
            payload: serde_json::json!({
                "repo": "org/repo",
                "branch": "main",
                "author": "alice",
            }),
            telemetry: None,
        });

        let blocks = payload
            .get("blocks")
            .and_then(Value::as_array)
            .expect("blocks");
        // label, divider, content, divider, fields, context
        assert!(blocks.len() >= 5);
        let fields = blocks[4]["fields"].as_array().expect("fields");
        assert!(fields.iter().any(|f| f["text"]
            .as_str()
            .map_or(false, |t| t.contains("repo"))));
        assert!(fields.iter().any(|f| f["text"]
            .as_str()
            .map_or(false, |t| t.contains("branch"))));
    }

    #[test]
    fn failed_event_uses_failed_label() {
        let payload = webhook_payload(&SinkMessage {
            event_kind: "github.ci-failed".into(),
            format: MessageFormat::Compact,
            content: "ci failed".into(),
            payload: serde_json::json!({}),
            telemetry: None,
        });

        let blocks = payload
            .get("blocks")
            .and_then(Value::as_array)
            .expect("blocks");
        assert_eq!(
            blocks[0]["text"]["text"].as_str(),
            Some(":x: *Failed*")
        );
    }
}
