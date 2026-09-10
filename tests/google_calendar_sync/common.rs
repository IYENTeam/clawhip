pub(crate) use std::collections::BTreeMap;
#[cfg(unix)]
pub(crate) use std::os::unix::fs::PermissionsExt;
pub(crate) use std::process::Stdio;
pub(crate) use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
pub(crate) use std::sync::{Arc, Mutex as StdMutex};
pub(crate) use std::time::Duration;

pub(crate) use axum::extract::{Query, State};
pub(crate) use axum::http::{HeaderMap, StatusCode};
pub(crate) use axum::response::{IntoResponse, Response};
pub(crate) use axum::routing::{get, post};
pub(crate) use axum::{Json, Router};
pub(crate) use serde_json::json;
pub(crate) use tempfile::TempDir;
pub(crate) use tokio::io::{AsyncBufReadExt, BufReader};
pub(crate) use tokio::net::TcpListener;
pub(crate) use tokio::process::{Child, Command};
pub(crate) use tokio::sync::{Notify, mpsc as tokio_mpsc};
pub(crate) use tokio::task::JoinHandle;
pub(crate) use tokio::time::timeout;

pub(crate) const TEST_EVENT_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const TEST_WATCH_EXPIRATION_MS: i64 = 4_102_444_800_000;

#[path = "process.rs"]
mod process;
pub(crate) use process::{spawn_daemon, spawn_daemon_with_proxy, write_calendar_config};

pub(crate) struct DaemonProcess {
    pub(crate) child: Child,
    pub(crate) stdout_task: Option<JoinHandle<()>>,
    pub(crate) proxy_task: Option<JoinHandle<()>>,
}

impl DaemonProcess {
    pub(crate) async fn stop(mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        if let Some(task) = self.stdout_task.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.proxy_task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        if let Some(task) = self.stdout_task.as_ref() {
            task.abort();
        }
        if let Some(task) = self.proxy_task.as_ref() {
            task.abort();
        }
    }
}

#[derive(Clone)]
pub(crate) struct FakeCalendarState {
    pub(crate) initial_requested: Arc<Notify>,
    pub(crate) initial_requests: Arc<AtomicUsize>,
    pub(crate) initial_failures_remaining: Arc<AtomicUsize>,
    pub(crate) release_initial: Arc<Notify>,
    pub(crate) incremental_requested: Arc<Notify>,
    pub(crate) recovery_full_requested: Arc<Notify>,
    pub(crate) recovery_requests: Arc<AtomicUsize>,
    pub(crate) recovery_failures_remaining: Arc<AtomicUsize>,
    pub(crate) recovery_succeeded: Arc<Notify>,
    pub(crate) expire_incremental: bool,
    pub(crate) incremental_seen: Arc<AtomicBool>,
    pub(crate) incremental_requests: Arc<AtomicUsize>,
    pub(crate) incremental_failures_remaining: Arc<AtomicUsize>,
    pub(crate) incremental_succeeded: Arc<Notify>,
    pub(crate) delivery_tx: tokio_mpsc::UnboundedSender<serde_json::Value>,
    pub(crate) sink_failures_remaining: Arc<AtomicUsize>,
    pub(crate) watch_requested: Arc<Notify>,
    pub(crate) watch_body: Arc<StdMutex<Option<serde_json::Value>>>,
    pub(crate) stop_requested: Arc<Notify>,
    pub(crate) stop_requests: Arc<AtomicUsize>,
    pub(crate) stop_failures_remaining: Arc<AtomicUsize>,
    pub(crate) stop_succeeded: Arc<Notify>,
    pub(crate) stop_body: Arc<StdMutex<Option<serde_json::Value>>>,
    pub(crate) fail_watch: bool,
    pub(crate) fail_incremental: bool,
    pub(crate) callback_during_watch: Option<String>,
    pub(crate) callback_completed: Arc<Notify>,
    pub(crate) callback_status: Arc<StdMutex<Option<u16>>>,
}

pub(crate) fn seed_active_watch(temp: &TempDir, expiration_ms: i64) {
    seed_watch(
        temp,
        "old-channel",
        "old-resource",
        "https://www.googleapis.com/calendar/v3/calendars/primary/events",
        expiration_ms,
        true,
    );
}

pub(crate) fn seed_tracked_watch(temp: &TempDir) {
    seed_watch(
        temp,
        "channel-1",
        "resource-1",
        "https://www.googleapis.com/calendar/v3/calendars/victor%40arkpoint.kr/events",
        current_time_millis() + 86_400_000,
        false,
    );
}

fn seed_watch(
    temp: &TempDir,
    channel_id: &str,
    resource_id: &str,
    resource_uri: &str,
    expiration_ms: i64,
    include_sync_token: bool,
) {
    let mut state = json!({
        "active_watch": {
            "id": channel_id,
            "resource_id": resource_id,
            "resource_uri": resource_uri,
            "expiration_ms": expiration_ms
        }
    });
    if include_sync_token {
        state["next_sync_token"] = json!("seed-sync-token");
    }
    let state_path = temp.path().join("calendar-state.json");
    std::fs::write(
        &state_path,
        serde_json::to_vec_pretty(&state).expect("serialize Calendar state fixture"),
    )
    .expect("write Calendar state fixture");
    #[cfg(unix)]
    std::fs::set_permissions(&state_path, std::fs::Permissions::from_mode(0o600))
        .expect("secure Calendar state fixture permissions");
}

pub(crate) fn current_time_millis() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_millis(),
    )
    .expect("current milliseconds fit i64")
}

pub(crate) fn calendar_headers() -> HeaderMap {
    calendar_headers_for("channel-1", "exists", 2)
}

pub(crate) fn calendar_headers_for(
    channel_id: &str,
    resource_state: &str,
    message_number: u64,
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("x-goog-channel-id", channel_id.parse().unwrap());
    headers.insert(
        "x-goog-channel-token",
        "test-channel-token-0123456789abcdef".parse().unwrap(),
    );
    headers.insert("x-goog-resource-id", "resource-1".parse().unwrap());
    headers.insert(
        "x-goog-resource-uri",
        "https://www.googleapis.com/calendar/v3/calendars/primary/events"
            .parse()
            .unwrap(),
    );
    headers.insert("x-goog-resource-state", resource_state.parse().unwrap());
    headers.insert(
        "x-goog-message-number",
        message_number.to_string().parse().unwrap(),
    );
    headers
}

pub(crate) fn pending_calendar_headers(
    channel_id: &str,
    resource_state: &str,
    message_number: u64,
) -> HeaderMap {
    let mut headers = calendar_headers_for(channel_id, resource_state, message_number);
    headers.insert("x-goog-resource-id", "resource-new".parse().unwrap());
    headers
}

pub(crate) fn fake_state(
    expire_incremental: bool,
) -> (
    FakeCalendarState,
    tokio_mpsc::UnboundedReceiver<serde_json::Value>,
) {
    let (delivery_tx, delivery_rx) = tokio_mpsc::unbounded_channel();
    (
        FakeCalendarState {
            initial_requested: Arc::new(Notify::new()),
            initial_requests: Arc::new(AtomicUsize::new(0)),
            initial_failures_remaining: Arc::new(AtomicUsize::new(0)),
            release_initial: Arc::new(Notify::new()),
            incremental_requested: Arc::new(Notify::new()),
            recovery_full_requested: Arc::new(Notify::new()),
            recovery_requests: Arc::new(AtomicUsize::new(0)),
            recovery_failures_remaining: Arc::new(AtomicUsize::new(0)),
            recovery_succeeded: Arc::new(Notify::new()),
            expire_incremental,
            incremental_seen: Arc::new(AtomicBool::new(false)),
            incremental_requests: Arc::new(AtomicUsize::new(0)),
            incremental_failures_remaining: Arc::new(AtomicUsize::new(0)),
            incremental_succeeded: Arc::new(Notify::new()),
            delivery_tx,
            sink_failures_remaining: Arc::new(AtomicUsize::new(0)),
            watch_requested: Arc::new(Notify::new()),
            watch_body: Arc::new(StdMutex::new(None)),
            stop_requested: Arc::new(Notify::new()),
            stop_requests: Arc::new(AtomicUsize::new(0)),
            stop_failures_remaining: Arc::new(AtomicUsize::new(0)),
            stop_succeeded: Arc::new(Notify::new()),
            stop_body: Arc::new(StdMutex::new(None)),
            fail_watch: false,
            fail_incremental: false,
            callback_during_watch: None,
            callback_completed: Arc::new(Notify::new()),
            callback_status: Arc::new(StdMutex::new(None)),
        },
        delivery_rx,
    )
}
