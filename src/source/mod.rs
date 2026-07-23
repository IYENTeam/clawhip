use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{RwLock, mpsc};

use crate::Result;
use crate::events::IncomingEvent;

pub mod discord_threads;
pub mod git;
pub mod github;
pub mod tmux;
pub mod workspace;

pub use discord_threads::DiscordThreadSource;
pub use git::GitSource;
pub use github::GitHubSource;
pub use tmux::{
    RegisteredTmuxSession, SharedTmuxRegistry, TmuxSource, list_active_tmux_registrations,
};
pub use workspace::WorkspaceSource;

#[async_trait::async_trait]
pub trait Source: Send + Sync {
    fn name(&self) -> &str;

    async fn run(&self, tx: mpsc::Sender<IncomingEvent>) -> Result<()>;
}

pub type SharedSourceHealth = Arc<RwLock<HashMap<String, SourceHealth>>>;

#[derive(Clone, Debug, Serialize)]
pub struct SourceHealth {
    pub status: String,
    pub started_at: Option<String>,
    pub last_heartbeat_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_error_at: Option<String>,
    pub last_error: Option<String>,
}

pub fn new_shared_source_health() -> SharedSourceHealth {
    Arc::new(RwLock::new(HashMap::new()))
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

pub async fn mark_source_started(health: &SharedSourceHealth, source: &str) {
    let now = now_rfc3339();
    let mut guard = health.write().await;
    let entry = guard
        .entry(source.to_string())
        .or_insert_with(|| SourceHealth {
            status: "starting".to_string(),
            started_at: None,
            last_heartbeat_at: None,
            last_success_at: None,
            last_error_at: None,
            last_error: None,
        });
    entry.status = "running".to_string();
    entry.started_at = Some(now.clone());
    entry.last_heartbeat_at = Some(now);
    entry.last_error = None;
}

pub async fn mark_source_success(health: &SharedSourceHealth, source: &str) {
    let now = now_rfc3339();
    let mut guard = health.write().await;
    let entry = guard
        .entry(source.to_string())
        .or_insert_with(|| SourceHealth {
            status: "running".to_string(),
            started_at: Some(now.clone()),
            last_heartbeat_at: None,
            last_success_at: None,
            last_error_at: None,
            last_error: None,
        });
    entry.status = "running".to_string();
    entry.last_heartbeat_at = Some(now.clone());
    entry.last_success_at = Some(now);
    entry.last_error = None;
}

pub async fn mark_source_error(health: &SharedSourceHealth, source: &str, error: impl ToString) {
    let now = now_rfc3339();
    let mut guard = health.write().await;
    let entry = guard
        .entry(source.to_string())
        .or_insert_with(|| SourceHealth {
            status: "degraded".to_string(),
            started_at: Some(now.clone()),
            last_heartbeat_at: None,
            last_success_at: None,
            last_error_at: None,
            last_error: None,
        });
    entry.status = "degraded".to_string();
    entry.last_heartbeat_at = Some(now.clone());
    entry.last_error_at = Some(now);
    entry.last_error = Some(error.to_string());
}

pub async fn mark_source_stopped(health: &SharedSourceHealth, source: &str, error: impl ToString) {
    let now = now_rfc3339();
    let mut guard = health.write().await;
    let entry = guard
        .entry(source.to_string())
        .or_insert_with(|| SourceHealth {
            status: "stopped".to_string(),
            started_at: None,
            last_heartbeat_at: None,
            last_success_at: None,
            last_error_at: None,
            last_error: None,
        });
    entry.status = "stopped".to_string();
    entry.last_heartbeat_at = Some(now.clone());
    entry.last_error_at = Some(now);
    entry.last_error = Some(error.to_string());
}

pub async fn mark_source_completed(health: &SharedSourceHealth, source: &str) {
    let now = now_rfc3339();
    let mut guard = health.write().await;
    let entry = guard
        .entry(source.to_string())
        .or_insert_with(|| SourceHealth {
            status: "completed".to_string(),
            started_at: None,
            last_heartbeat_at: None,
            last_success_at: None,
            last_error_at: None,
            last_error: None,
        });
    entry.status = "completed".to_string();
    entry.last_heartbeat_at = Some(now);
}

#[derive(Default)]
pub struct ErrorLogDeduper {
    last_by_key: HashMap<String, String>,
}

impl ErrorLogDeduper {
    pub fn should_log(&mut self, key: &str, error: &str) -> bool {
        if self
            .last_by_key
            .get(key)
            .is_some_and(|previous| previous == error)
        {
            return false;
        }
        self.last_by_key.insert(key.to_string(), error.to_string());
        true
    }

    pub fn clear(&mut self, key: &str) {
        self.last_by_key.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::ErrorLogDeduper;

    #[test]
    fn error_log_deduper_emits_once_until_error_changes_or_recovers() {
        let mut deduper = ErrorLogDeduper::default();

        assert!(deduper.should_log("github", "404"));
        assert!(!deduper.should_log("github", "404"));
        assert!(deduper.should_log("github", "500"));
        assert!(deduper.should_log("discord-threads", "404"));

        deduper.clear("github");
        assert!(deduper.should_log("github", "404"));
    }
}
