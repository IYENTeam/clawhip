use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::sleep;

use crate::Result;
use crate::config::AppConfig;
use crate::events::{IncomingEvent, MessageFormat};
use crate::source::Source;

pub struct OpenCodeSource {
    config: Arc<AppConfig>,
}

impl OpenCodeSource {
    pub fn new(config: Arc<AppConfig>) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl Source for OpenCodeSource {
    fn name(&self) -> &str {
        "opencode"
    }

    async fn run(&self, tx: mpsc::Sender<IncomingEvent>) -> Result<()> {
        let url = match &self.config.monitors.opencode.url {
            Some(url) => url.clone(),
            None => return Ok(()), // no config → silent exit
        };

        let poll_interval = Duration::from_secs(
            self.config.monitors.opencode.poll_interval_secs.max(1),
        );
        let idle_threshold = Duration::from_secs(
            self.config.monitors.opencode.idle_threshold_secs,
        );
        let channel = self.config.monitors.opencode.channel.clone();
        let mention = self.config.monitors.opencode.mention.clone();
        let format = self.config.monitors.opencode.format.clone();

        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| format!("opencode http client: {e}"))?;

        let mut state = OpenCodeState::default();

        loop {
            if let Err(e) = poll_opencode(
                &client, &url, &tx, &mut state,
                idle_threshold, &channel, &mention, &format,
            ).await {
                eprintln!("clawhip opencode poll error: {e}");
            }
            sleep(poll_interval).await;
        }
    }
}

#[derive(Default)]
struct OpenCodeState {
    known_sessions: HashMap<String, SessionSnapshot>,
    idle_alerted: HashSet<String>,
    warmed_up: bool,
}

struct SessionSnapshot {
    updated_ms: u64,
    message_count: usize,
    title: String,
}

#[derive(Deserialize)]
struct SessionInfo {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    time: SessionTime,
    #[serde(default)]
    summary: Option<Value>,
}

#[derive(Deserialize, Default)]
struct SessionTime {
    #[serde(default)]
    created: u64,
    #[serde(default)]
    updated: u64,
}

#[derive(Deserialize)]
struct SessionMessage {
    #[serde(default)]
    role: String,
    #[serde(default)]
    parts: Vec<MessagePart>,
}

#[derive(Deserialize)]
struct MessagePart {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(rename = "toolInvocation", default)]
    tool_invocation: Option<Value>,
}

async fn poll_opencode(
    client: &Client,
    base_url: &str,
    tx: &mpsc::Sender<IncomingEvent>,
    state: &mut OpenCodeState,
    idle_threshold: Duration,
    channel: &Option<String>,
    mention: &Option<String>,
    format: &Option<MessageFormat>,
) -> Result<()> {
    let sessions: Vec<SessionInfo> = client
        .get(format!("{base_url}/session"))
        .send()
        .await
        .map_err(|e| format!("opencode list sessions: {e}"))?
        .json()
        .await
        .map_err(|e| format!("opencode parse sessions: {e}"))?;

    let current_ids: HashSet<String> = sessions.iter().map(|s| s.id.clone()).collect();

    let is_warmup = !state.warmed_up;

    // Detect ended sessions (skip during warmup)
    if !is_warmup {
        let ended: Vec<String> = state.known_sessions.keys()
            .filter(|id| !current_ids.contains(*id))
            .cloned()
            .collect();
        for id in ended {
            let snap = state.known_sessions.remove(&id).unwrap();
            state.idle_alerted.remove(&id);
            let event = make_event(
                "opencode.session.ended",
                json!({
                    "session_id": id,
                    "title": snap.title,
                    "summary": "opencode session ended",
                }),
                channel,
                mention,
                format,
            );
            let _ = tx.send(event).await;
        }
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    for session in sessions {
        let is_new = !state.known_sessions.contains_key(&session.id);

        if is_new {
            state.idle_alerted.remove(&session.id);

            // Only emit created event after warmup
            if !is_warmup {
                let event = make_event(
                    "opencode.session.created",
                    json!({
                        "session_id": &session.id,
                        "title": &session.title,
                        "summary": format!("new session: {}", session.title),
                    }),
                    channel,
                    mention,
                    format,
                );
                let _ = tx.send(event).await;
            }

            // Fetch current message count so we don't replay old messages
            let msg_count = fetch_messages(client, base_url, &session.id).await
                .map(|msgs| msgs.len())
                .unwrap_or(0);

            state.known_sessions.insert(session.id.clone(), SessionSnapshot {
                updated_ms: session.time.updated,
                message_count: msg_count,
                title: session.title.clone(),
            });

            // During warmup, mark already-idle sessions so we don't alert
            if is_warmup {
                let elapsed = now_ms.saturating_sub(session.time.updated);
                if elapsed > idle_threshold.as_millis() as u64 {
                    state.idle_alerted.insert(session.id.clone());
                }
            }
        }

        let snap = state.known_sessions.get_mut(&session.id).unwrap();

        // Check for updates
        if session.time.updated > snap.updated_ms {
            snap.updated_ms = session.time.updated;
            snap.title = session.title.clone();
            state.idle_alerted.remove(&session.id);

            // Fetch messages to see what changed
            if let Ok(messages) = fetch_messages(client, base_url, &session.id).await {
                let new_count = messages.len();
                if new_count > snap.message_count {
                    // Report new messages
                    for msg in messages.iter().skip(snap.message_count) {
                        if msg.role == "assistant" {
                            let text = msg.parts.iter()
                                .filter_map(|p| {
                                    if p.kind == "text" { p.text.clone() }
                                    else { None }
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            let tools: Vec<String> = msg.parts.iter()
                                .filter_map(|p| {
                                    if p.kind == "tool-invocation" {
                                        p.tool_invocation.as_ref()
                                            .and_then(|ti| ti.get("toolName"))
                                            .and_then(|v| v.as_str())
                                            .map(String::from)
                                    } else { None }
                                })
                                .collect();

                            if !tools.is_empty() {
                                let event = make_event(
                                    "opencode.message.tool",
                                    json!({
                                        "session_id": &session.id,
                                        "title": &session.title,
                                        "tools": tools,
                                        "summary": format!("tools: {}", tools.join(", ")),
                                    }),
                                    channel,
                                    mention,
                                    format,
                                );
                                let _ = tx.send(event).await;
                            }

                            if !text.is_empty() {
                                let truncated = if text.len() > 200 {
                                    format!("{}…", &text[..200])
                                } else {
                                    text.clone()
                                };
                                let event = make_event(
                                    "opencode.message.assistant",
                                    json!({
                                        "session_id": &session.id,
                                        "title": &session.title,
                                        "text": truncated,
                                        "summary": format!("assistant: {}", if text.len() > 80 { &text[..80] } else { &text }),
                                    }),
                                    channel,
                                    mention,
                                    format,
                                );
                                let _ = tx.send(event).await;
                            }
                        }
                    }
                    snap.message_count = new_count;
                }
            }
        }

        // Idle detection
        let elapsed_ms = now_ms.saturating_sub(snap.updated_ms);
        if elapsed_ms > idle_threshold.as_millis() as u64 && !state.idle_alerted.contains(&session.id) {
            state.idle_alerted.insert(session.id.clone());
            let idle_mins = elapsed_ms / 60_000;
            let event = make_event(
                "opencode.session.idle",
                json!({
                    "session_id": &session.id,
                    "title": &session.title,
                    "idle_minutes": idle_mins,
                    "summary": format!("session idle for {}m: {}", idle_mins, session.title),
                }),
                channel,
                mention,
                format,
            );
            let _ = tx.send(event).await;
        }
    }

    if is_warmup {
        state.warmed_up = true;
        eprintln!("clawhip opencode warmup complete: {} existing sessions", state.known_sessions.len());
    }

    Ok(())
}

async fn fetch_messages(
    client: &Client,
    base_url: &str,
    session_id: &str,
) -> Result<Vec<SessionMessage>> {
    let messages: Vec<SessionMessage> = client
        .get(format!("{base_url}/session/{session_id}/message"))
        .send()
        .await
        .map_err(|e| format!("opencode messages: {e}"))?
        .json()
        .await
        .map_err(|e| format!("opencode parse messages: {e}"))?;
    Ok(messages)
}

fn make_event(
    kind: &str,
    payload: Value,
    channel: &Option<String>,
    mention: &Option<String>,
    format: &Option<MessageFormat>,
) -> IncomingEvent {
    IncomingEvent::workspace(kind.to_string(), payload, channel.clone())
        .with_mention(mention.clone())
        .with_format(format.clone())
}
