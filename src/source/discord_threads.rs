use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::{MissedTickBehavior, interval};

use crate::Result;
use crate::config::{AppConfig, DiscordThreadMonitor};
use crate::events::IncomingEvent;
use crate::source::{ErrorLogDeduper, Source};

pub struct DiscordThreadSource {
    config: Arc<AppConfig>,
}

impl DiscordThreadSource {
    pub fn new(config: Arc<AppConfig>) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl Source for DiscordThreadSource {
    fn name(&self) -> &str {
        "discord-threads"
    }

    async fn run(&self, tx: mpsc::Sender<IncomingEvent>) -> Result<()> {
        if self.config.monitors.discord_threads.is_empty() {
            return Ok(());
        }

        let Some(token) = self.config.effective_token() else {
            eprintln!("op-pi source discord-threads disabled: missing Discord bot token");
            return Ok(());
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bot {token}"))?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()?;
        let api_base = std::env::var("OP_PI_DISCORD_API_BASE")
            .or_else(|_| std::env::var("CLAWHIP_DISCORD_API_BASE"))
            .unwrap_or_else(|_| "https://discord.com/api/v10".to_string());

        let mut known = HashSet::new();
        let mut error_logs = ErrorLogDeduper::default();
        for monitor in &self.config.monitors.discord_threads {
            match fetch_active_thread_ids(&client, &api_base, &monitor.parent_channel).await {
                Ok(ids) => {
                    error_logs.clear(&monitor.parent_channel);
                    known.extend(ids);
                }
                Err(error) => {
                    let error = error.to_string();
                    if error_logs.should_log(&monitor.parent_channel, &error) {
                        eprintln!(
                            "op-pi source discord-threads failed scan for {}: {error}",
                            monitor.parent_channel
                        );
                    }
                }
            }
        }

        let mut tick = interval(global_poll_interval(&self.config.monitors.discord_threads));
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            tick.tick().await;
            for monitor in &self.config.monitors.discord_threads {
                let threads =
                    match fetch_active_threads(&client, &api_base, &monitor.parent_channel).await {
                        Ok(threads) => {
                            error_logs.clear(&monitor.parent_channel);
                            threads
                        }
                        Err(error) => {
                            let error = error.to_string();
                            if error_logs.should_log(&monitor.parent_channel, &error) {
                                eprintln!(
                                    "op-pi source discord-threads scan failed for {}: {error}",
                                    monitor.parent_channel
                                );
                            }
                            continue;
                        }
                    };

                for thread in threads {
                    if !known.insert(thread.id.clone()) {
                        continue;
                    }

                    let message = render_thread_trigger(monitor, &thread);
                    let event = IncomingEvent {
                        kind: "custom".to_string(),
                        channel: Some(thread.id.clone()),
                        mention: None,
                        format: None,
                        template: None,
                        payload: json!({
                            "message": message,
                            "thread_id": thread.id,
                            "thread_name": thread.name,
                            "parent_channel": monitor.parent_channel,
                            "source": "discord.thread-created",
                        }),
                    };
                    if tx.send(event).await.is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct ActiveThreadsResponse {
    #[serde(default)]
    threads: Vec<DiscordThread>,
}

#[derive(Debug, Deserialize)]
struct DiscordThread {
    id: String,
    #[serde(default)]
    guild_id: Option<String>,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

async fn fetch_active_thread_ids(
    client: &reqwest::Client,
    api_base: &str,
    parent_channel: &str,
) -> Result<HashSet<String>> {
    Ok(fetch_active_threads(client, api_base, parent_channel)
        .await?
        .into_iter()
        .map(|thread| thread.id)
        .collect())
}

async fn fetch_channel(
    client: &reqwest::Client,
    api_base: &str,
    channel_id: &str,
) -> Result<DiscordThread> {
    let url = format!("{}/channels/{}", api_base.trim_end_matches('/'), channel_id);
    let response = client.get(url).send().await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Discord channel request failed with {status}: {body}").into());
    }
    Ok(response.json::<DiscordThread>().await?)
}

async fn fetch_active_threads(
    client: &reqwest::Client,
    api_base: &str,
    parent_channel: &str,
) -> Result<Vec<DiscordThread>> {
    let parent = fetch_channel(client, api_base, parent_channel).await?;
    let Some(guild_id) = parent.guild_id else {
        return Err("Discord thread monitor parent has no guild_id".into());
    };
    let url = format!(
        "{}/guilds/{}/threads/active",
        api_base.trim_end_matches('/'),
        guild_id
    );
    let response = client.get(url).send().await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Discord active threads request failed with {status}: {body}").into());
    }
    let body = response.json::<ActiveThreadsResponse>().await?;
    Ok(body
        .threads
        .into_iter()
        .filter(|thread| thread.parent_id.as_deref() == Some(parent_channel))
        .collect())
}

fn global_poll_interval(monitors: &[DiscordThreadMonitor]) -> Duration {
    let secs = monitors
        .iter()
        .filter_map(|monitor| monitor.poll_interval_secs)
        .min()
        .unwrap_or(5)
        .max(1);
    Duration::from_secs(secs)
}

fn render_thread_trigger(monitor: &DiscordThreadMonitor, thread: &DiscordThread) -> String {
    let mention = monitor.mention.trim();
    let thread_name = thread.name.as_deref().unwrap_or("untitled thread");
    let template = monitor.message.as_deref().unwrap_or(
        "{mention} 작업 쓰레드가 생성되었습니다. 이 쓰레드 컨텍스트에서 티켓 intro/init을 확인하고 작업을 시작하세요.\nthread: {thread_name}\nthread_id: {thread_id}\nparent_channel: {parent_channel}",
    );
    template
        .replace("{mention}", mention)
        .replace("{thread_name}", thread_name)
        .replace("{thread_id}", &thread.id)
        .replace("{parent_channel}", &monitor.parent_channel)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor() -> DiscordThreadMonitor {
        DiscordThreadMonitor {
            parent_channel: "1506217431465463929".into(),
            parent_channel_name: Some("task-request".into()),
            mention: "<@1486621536520769547>".into(),
            message: None,
            poll_interval_secs: None,
        }
    }

    #[test]
    fn default_trigger_mentions_agent_and_thread_context() {
        let thread = DiscordThread {
            id: "1509517124157313127".into(),
            guild_id: Some("guild".into()),
            parent_id: Some("1506217431465463929".into()),
            name: Some("T-20260528-020 smoke verify".into()),
        };

        let rendered = render_thread_trigger(&monitor(), &thread);

        assert!(rendered.contains("<@1486621536520769547> 작업 쓰레드가 생성되었습니다"));
        assert!(rendered.contains("티켓 intro/init을 확인"));
        assert!(rendered.contains("thread: T-20260528-020 smoke verify"));
        assert!(rendered.contains("thread_id: 1509517124157313127"));
        assert!(rendered.contains("parent_channel: 1506217431465463929"));
    }

    #[test]
    fn custom_trigger_template_expands_placeholders() {
        let mut monitor = monitor();
        monitor.message = Some("{mention}|{thread_name}|{thread_id}|{parent_channel}".into());
        let thread = DiscordThread {
            id: "thread-1".into(),
            guild_id: None,
            parent_id: None,
            name: Some("ticket thread".into()),
        };

        assert_eq!(
            render_thread_trigger(&monitor, &thread),
            "<@1486621536520769547>|ticket thread|thread-1|1506217431465463929"
        );
    }

    #[test]
    fn global_poll_interval_uses_lowest_positive_interval_or_default() {
        let mut slow = monitor();
        slow.poll_interval_secs = Some(10);
        let mut fast = monitor();
        fast.poll_interval_secs = Some(2);

        assert_eq!(global_poll_interval(&[]), Duration::from_secs(5));
        assert_eq!(global_poll_interval(&[slow, fast]), Duration::from_secs(2));
    }
}
