use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router as AxumRouter};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{Mutex, RwLock, Semaphore, mpsc, oneshot};

use crate::Result;
use crate::VERSION;
use crate::calendar::journal::persist_deferred_notification;
use crate::calendar::{CalendarNotification, DeferredCalendarNotification};
use crate::config::{AppConfig, GajaeRouteAction, RouteRule};
use crate::core::rate_limit::RateLimiter;
use crate::cron::CronSource;
use crate::dispatch::Dispatcher;
use crate::event::compat::from_incoming_event;
use crate::events::{IncomingEvent, MessageFormat, normalize_event};
use crate::gajae::{HandlerAction, HandlerLimits, HandlerOutcome};
use crate::native_hooks::{
    NATIVE_NON_GIT_OUTCOME, NATIVE_NORMALIZATION_OUTCOME_FIELD,
    incoming_event_from_native_hook_json,
};
use crate::native_observability::{
    SharedNativeHookObservability, is_native_hook_event, native_event_telemetry_fields,
    new_shared_native_hook_observability, snapshot_shared, with_native_observability,
};
use crate::render::{DefaultRenderer, Renderer};
use crate::router::Router;
use crate::sink::{DiscordSink, LocalFileSink, Sink, SlackSink};
use crate::source::{
    DiscordThreadSource, GitHubSource, GitSource, GoogleCalendarSource, RegisteredTmuxSession,
    SharedSourceHealth, SharedTmuxRegistry, Source, TmuxSource, WorkspaceSource,
    list_active_tmux_registrations, mark_source_completed, mark_source_started,
    mark_source_stopped, new_shared_source_health,
};
use crate::telemetry;
use crate::update::{self, SharedPendingUpdate};

const EVENT_QUEUE_CAPACITY: usize = 256;
const LINEAR_REPLAY_TTL_MS: u64 = 60_000;
const LINEAR_REPLAY_CAPACITY: usize = 1_024;
const LINEAR_READ_TIMEOUT: Duration = Duration::from_secs(5);
const LINEAR_MAX_CONCURRENT_READS: usize = 32;
const LINEAR_DELIVERY_MAX_BYTES: usize = 1_024;
const LINEAR_RATE_BURST: u32 = 128;
const LINEAR_RATE_PER_SEC: f64 = 64.0;
const CALENDAR_PERSIST_ACK_TIMEOUT: Duration = Duration::from_millis(250);
const CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY: u32 = 32;
const CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC: f64 = 8.0;
const CALENDAR_WEBHOOK_RATE_LIMIT_KEY: &str = "google-calendar";
const STALE_NATIVE_REPLAY_GRACE: Duration = Duration::from_secs(5 * 60);
const STALE_NATIVE_REPLAY_REASON: &str = "stale_replay";
const NATIVE_REPLAY_TIMESTAMP_POINTERS: &[&str] = &[
    "/event_timestamp",
    "/timestamp",
    "/observed_at",
    "/created_at",
    "/event_payload/event_timestamp",
    "/event_payload/timestamp",
];
const EVENT_REPLAY_TIMESTAMP_POINTERS: &[&str] = &[
    "/first_seen_at",
    "/event_timestamp",
    "/timestamp",
    "/observed_at",
    "/created_at",
];

#[derive(Clone)]
struct LinearIntake {
    replay: Arc<StdMutex<LinearReplayCache>>,
    permits: Arc<Semaphore>,
    now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
    read_timeout: Duration,
    rate: Arc<StdMutex<LinearRateLimiter>>,
}

struct LinearReplayCache {
    entries: VecDeque<([u8; 32], u64)>,
}

struct LinearRateLimiter {
    tokens: f64,
    last_ms: u64,
}

impl LinearIntake {
    fn production() -> Self {
        Self::new(
            LINEAR_MAX_CONCURRENT_READS,
            LINEAR_READ_TIMEOUT,
            unix_timestamp_ms,
        )
    }

    fn new(
        max_concurrent_reads: usize,
        read_timeout: Duration,
        now_ms: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Self {
        let initial_now = now_ms();
        Self {
            replay: Arc::new(StdMutex::new(LinearReplayCache {
                entries: VecDeque::new(),
            })),
            permits: Arc::new(Semaphore::new(max_concurrent_reads)),
            now_ms: Arc::new(now_ms),
            read_timeout,
            rate: Arc::new(StdMutex::new(LinearRateLimiter {
                tokens: f64::from(LINEAR_RATE_BURST),
                last_ms: initial_now,
            })),
        }
    }

    fn replay_identity(body: &[u8]) -> [u8; 32] {
        use sha2::Digest as _;
        sha2::Sha256::digest(body).into()
    }
}

impl LinearReplayCache {
    fn prune(&mut self, now_ms: u64) {
        self.entries.retain(|(_, expires_at)| *expires_at >= now_ms);
    }

    fn contains(&self, identity: &[u8; 32]) -> bool {
        self.entries.iter().any(|(entry, _)| entry == identity)
    }

    fn insert(&mut self, identity: [u8; 32], expires_at: u64) {
        self.entries.push_back((identity, expires_at));
    }
}

impl LinearRateLimiter {
    fn try_consume(&mut self, now_ms: u64) -> bool {
        let elapsed_ms = now_ms.saturating_sub(self.last_ms);
        self.tokens = (self.tokens + elapsed_ms as f64 * LINEAR_RATE_PER_SEC / 1_000.0)
            .min(f64::from(LINEAR_RATE_BURST));
        self.last_ms = now_ms;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

#[derive(Clone)]
struct AppState {
    config: Arc<AppConfig>,
    port: u16,
    tx: mpsc::Sender<IncomingEvent>,
    tmux_registry: SharedTmuxRegistry,
    pending_update: SharedPendingUpdate,
    native_observability: SharedNativeHookObservability,
    source_health: SharedSourceHealth,
    cron_state_path: PathBuf,
    discord_watch_lock: Arc<Mutex<()>>,
    sns_cert_cache: Arc<crate::intake::SnsCertCache>,
    calendar_webhook_rate_limit: Arc<Mutex<RateLimiter>>,
}

pub async fn run(
    config: Arc<AppConfig>,
    port_override: Option<u16>,
    cron_state_path: PathBuf,
) -> Result<()> {
    config.validate()?;
    let token_source = config.discord_token_source();
    println!("op_pi v{VERSION} starting (token_source: {token_source})");
    telemetry::emit(daemon_record(
        telemetry::reason::DAEMON_STARTUP,
        json!({"version": VERSION, "token_source": token_source}),
    ));
    if let Some(env_var) = config.discord_token_env_shadow() {
        let warning = discord_token_shadow_warning(env_var);
        eprintln!("warning: {warning}");
        telemetry::emit(daemon_record(
            telemetry::reason::DISCORD_TOKEN_ENV_SHADOW,
            json!({"env_var": env_var, "token_source": token_source, "warning": warning}),
        ));
    }
    let port = port_override.unwrap_or(config.daemon.port);
    let addr: SocketAddr = format!("{}:{}", config.daemon.bind_host, port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local_addr = listener.local_addr()?;
    let mut sinks: HashMap<String, Box<dyn Sink>> = HashMap::new();
    sinks.insert(
        "discord".into(),
        Box::new(DiscordSink::from_config(config.clone())?),
    );
    sinks.insert(
        "slack".into(),
        Box::new(SlackSink::from_config(config.clone())?),
    );
    sinks.insert("localfile".into(), Box::new(LocalFileSink));
    let renderer: Box<dyn Renderer> = Box::new(DefaultRenderer);
    let router = Router::new(config.clone());
    let tmux_registry: SharedTmuxRegistry = Arc::new(RwLock::new(HashMap::new()));
    let (tx, rx) = mpsc::channel(EVENT_QUEUE_CAPACITY);
    let (calendar_notification_tx, calendar_notification_rx) = mpsc::channel(EVENT_QUEUE_CAPACITY);
    let (calendar_receipt_tx, calendar_receipt_rx) = mpsc::channel(EVENT_QUEUE_CAPACITY);
    let native_observability = new_shared_native_hook_observability();
    let source_health = new_shared_source_health();

    let ci_batch_window = config.dispatch.ci_batch_window();
    let routine_batch_window = config.dispatch.routine_batch_window();
    let dispatcher_native_observability = native_observability.clone();
    tokio::spawn(async move {
        let mut dispatcher = Dispatcher::new(
            rx,
            router,
            renderer,
            sinks,
            ci_batch_window,
            routine_batch_window,
            dispatcher_native_observability,
        )
        .with_calendar_receipts(calendar_receipt_tx);
        if let Err(error) = dispatcher.run().await {
            eprintln!("op_pi dispatcher stopped: {error}");
        }
    });
    spawn_source(
        GitSource::new(config.clone()),
        tx.clone(),
        source_health.clone(),
    );
    spawn_source(
        GitHubSource::new(config.clone(), source_health.clone()),
        tx.clone(),
        source_health.clone(),
    );
    spawn_source(
        TmuxSource::new(config.clone(), tmux_registry.clone()),
        tx.clone(),
        source_health.clone(),
    );
    spawn_source(
        WorkspaceSource::new(config.clone()),
        tx.clone(),
        source_health.clone(),
    );
    spawn_source(
        DiscordThreadSource::new(config.clone()),
        tx.clone(),
        source_health.clone(),
    );
    spawn_source(
        CronSource::new(config.clone(), cron_state_path.clone()),
        tx.clone(),
        source_health.clone(),
    );
    spawn_source(
        GoogleCalendarSource::new(
            config.clone(),
            source_health.clone(),
            calendar_notification_rx,
            calendar_receipt_rx,
        ),
        tx.clone(),
        source_health.clone(),
    );

    let pending_update = update::new_shared_pending_update();
    {
        let config = config.clone();
        let tx = tx.clone();
        let pending = pending_update.clone();
        tokio::spawn(async move {
            update::run_checker(config, tx, pending).await;
        });
    }

    let ledger = match std::env::var("OP_PI_DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => {
            let ledger = evidence_ledger::EvidenceLedger::connect(&url).await?;
            ledger.migrate().await?;
            Some(ledger)
        }
        _ => None,
    };
    let app = app_router_with_calendar(
        AppState {
            config: config.clone(),
            port,
            tx,
            tmux_registry,
            pending_update,
            native_observability,
            source_health,
            cron_state_path,
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        },
        calendar_notification_tx,
        LinearIntake::production(),
        ledger,
    );
    println!(
        "op_pi daemon v{VERSION} listening on http://{} (token_source: {token_source})",
        local_addr
    );
    telemetry::emit(daemon_record(
        telemetry::reason::DAEMON_LISTENING,
        json!({"version": VERSION, "addr": local_addr.to_string(), "token_source": token_source}),
    ));
    axum::serve(listener, app).await?;
    Ok(())
}

fn app_router_with_calendar(
    state: AppState,
    calendar_notification_tx: mpsc::Sender<CalendarNotification>,
    linear: LinearIntake,
    ledger: Option<evidence_ledger::EvidenceLedger>,
) -> AxumRouter {
    AxumRouter::new()
        .route("/health", get(health))
        .route("/api/status", get(status))
        .route("/event", post(post_event))
        .route("/api/event", post(post_event))
        .route("/events", post(post_event))
        .route("/native/hook", post(post_native_hook))
        .route("/api/native/hook", post(post_native_hook))
        .route("/api/tmux/register", post(register_tmux))
        .route("/api/tmux", get(list_tmux))
        .route("/github", post(post_github))
        .route("/aws/sns", post(post_aws_sns))
        .route("/aws/eventbridge", post(post_aws_eventbridge))
        .route("/cloudflare", post(post_cloudflare_notification))
        .route(
            "/cloudflare/logpush",
            post(post_cloudflare_logpush).layer(axum::extract::DefaultBodyLimit::max(
                crate::intake::LOGPUSH_MAX_BODY_BYTES,
            )),
        )
        .route("/linear", post(post_linear).layer(Extension(linear)))
        .route(
            "/google/calendar",
            post(post_google_calendar_with_source).layer(Extension(calendar_notification_tx)),
        )
        .route("/api/update/status", get(update_status))
        .route("/api/update/approve", post(approve_update))
        .route("/api/update/dismiss", post(dismiss_update))
        .with_state(state)
        .layer(Extension(ledger))
}

#[cfg(test)]
fn app_router(state: AppState) -> AxumRouter {
    let (calendar_notification_tx, _calendar_notification_rx) = mpsc::channel(1);
    app_router_with_calendar(
        state,
        calendar_notification_tx,
        LinearIntake::production(),
        None,
    )
}

fn spawn_source<S>(source: S, tx: mpsc::Sender<IncomingEvent>, source_health: SharedSourceHealth)
where
    S: Source + Send + Sync + 'static,
{
    let source_name = source.name().to_string();
    tokio::spawn(async move {
        println!("op_pi source '{}' starting", source_name);
        mark_source_started(&source_health, &source_name).await;
        telemetry::emit(source_lifecycle_record(
            telemetry::reason::SOURCE_START,
            &source_name,
            None,
        ));
        match source.run(tx.clone()).await {
            Ok(()) => {
                mark_source_completed(&source_health, &source_name).await;
            }
            Err(error) => {
                mark_source_stopped(&source_health, &source_name, error.to_string()).await;
                telemetry::emit(source_lifecycle_record(
                    telemetry::reason::SOURCE_STOPPED,
                    &source_name,
                    Some(error.to_string()),
                ));
                eprintln!("op_pi source '{}' stopped: {error}", source_name);
                if let Err(alert_error) = tx
                    .send(source_failure_alert_event(&source_name, &error.to_string()))
                    .await
                {
                    eprintln!(
                        "op_pi source '{}' could not enqueue degraded alert: {alert_error}",
                        source_name
                    );
                }
            }
        }
    });
}

fn source_failure_alert_event(source_name: &str, error_message: &str) -> IncomingEvent {
    let mut event = IncomingEvent::custom(
        None,
        format!("op_pi degraded: source '{source_name}' stopped: {error_message}"),
    )
    .with_format(Some(MessageFormat::Alert));

    if let Some(payload) = event.payload.as_object_mut() {
        payload.insert("source_name".to_string(), json!(source_name));
        payload.insert("health_status".to_string(), json!("degraded"));
        payload.insert("error_message".to_string(), json!(error_message));
    }

    event
}

fn daemon_record(reason_code: &str, details: Value) -> serde_json::Map<String, Value> {
    let mut record = telemetry::record(
        telemetry::event_name::DAEMON_PHASE,
        reason_code,
        format!("daemon:{reason_code}"),
    );
    record.insert("details".to_string(), details);
    record
}

fn discord_token_shadow_warning(env_var: &str) -> String {
    format!(
        "Discord token from environment variable {env_var} is overriding the token configured in the config file (token_source: env). Unset {env_var} to use the configured token, or remove the config token to silence this notice."
    )
}

fn source_lifecycle_record(
    reason_code: &str,
    source_name: &str,
    error: Option<String>,
) -> serde_json::Map<String, Value> {
    let event_name = if reason_code == telemetry::reason::SOURCE_STOPPED {
        telemetry::event_name::SOURCE_DEGRADED
    } else {
        telemetry::event_name::SOURCE_INVENTORY
    };
    let mut record = telemetry::record(event_name, reason_code, format!("source:{source_name}"));
    record.insert("source".to_string(), json!(source_name));
    if let Some(error) = error {
        record.insert("error".to_string(), json!(error));
    }
    record
}

fn event_record(
    event_name: &str,
    reason_code: &str,
    event: &IncomingEvent,
    details: Value,
) -> serde_json::Map<String, Value> {
    let mut record = telemetry::record(
        event_name,
        reason_code,
        telemetry::correlation_id_for_event(event),
    );
    record.insert("event_kind".to_string(), json!(event.canonical_kind()));
    record.insert("details".to_string(), details);
    record
}

fn parse_rfc3339_timestamp(value: Option<&str>) -> Option<OffsetDateTime> {
    value.and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
}

fn source_health_is_ok(config: &AppConfig, sources: &Value) -> bool {
    let Some(map) = sources.as_object() else {
        return false;
    };
    if config.google_calendar.sync_enabled()
        && map
            .get("google-calendar")
            .and_then(Value::as_object)
            .and_then(|source| source.get("status"))
            .and_then(Value::as_str)
            != Some("running")
    {
        return false;
    }
    if config
        .monitors
        .git
        .repos
        .iter()
        .any(|repo| repo.emit_issue_opened || repo.emit_pr_status)
    {
        let Some(github) = map.get("github").and_then(Value::as_object) else {
            return false;
        };
        if github.get("status").and_then(Value::as_str) != Some("running") {
            return false;
        }
        let heartbeat =
            parse_rfc3339_timestamp(github.get("last_heartbeat_at").and_then(Value::as_str));
        let Some(heartbeat) = heartbeat else {
            return false;
        };
        let allowed_age = Duration::from_secs(config.monitors.poll_interval_secs.max(1) + 120);
        let age = OffsetDateTime::now_utc() - heartbeat;
        if age > allowed_age {
            return false;
        }
    }
    true
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let registered = state.tmux_registry.read().await.len();
    let native_hooks = snapshot_shared(&state.native_observability);
    let sources = serde_json::to_value(state.source_health.read().await.clone())
        .unwrap_or_else(|_| json!({}));
    Json(health_payload(
        state.config.as_ref(),
        state.port,
        registered,
        native_hooks,
        sources,
    ))
}

fn health_payload(
    config: &AppConfig,
    port: u16,
    registered_tmux_sessions: usize,
    native_hooks: Value,
    sources: Value,
) -> Value {
    let sources_ok = source_health_is_ok(config, &sources);
    json!({
        "ok": sources_ok,
        "version": VERSION,
        "token_source": config.discord_token_source(),
        "token_precedence_warning": config.discord_token_env_shadow().map(discord_token_shadow_warning),
        "webhook_routes_configured": config.has_webhook_routes(),
        "port": port,
        "daemon_base_url": config.daemon.base_url,
        "configured_git_monitors": config.monitors.git.repos.len(),
        "configured_tmux_monitors": config.monitors.tmux.sessions.len(),
        "configured_workspace_monitors": config.monitors.workspace.len(),
        "configured_discord_thread_monitors": config.monitors.discord_threads.len(),
        "configured_cron_jobs": config.cron.jobs.len(),
        "registered_tmux_sessions": registered_tmux_sessions,
        "native_hooks": native_hooks,
        "sources": sources,
    })
}

async fn status(State(state): State<AppState>) -> impl IntoResponse {
    health(State(state)).await
}

async fn post_event(
    State(state): State<AppState>,
    Json(event): Json<IncomingEvent>,
) -> impl IntoResponse {
    let canonical_kind = event.canonical_kind();
    if matches!(
        canonical_kind,
        "google.calendar.changed" | "google.calendar.sync"
    ) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "ok": false,
                "error": "Google Calendar callbacks must use /google/calendar",
            })),
        )
            .into_response();
    }
    if let Some(defer) = stale_replay_defer(
        canonical_kind,
        &event.payload,
        EVENT_REPLAY_TIMESTAMP_POINTERS,
    ) {
        return stale_replay_defer_response(canonical_kind, &defer);
    }

    accept_event(&state, normalize_event(event)).await
}

async fn post_native_hook(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let raw_non_git = native_payload_is_non_git(&payload);
    with_native_observability(&state.native_observability, |observability| {
        observability.observe_received_raw(&payload);
    });
    eprintln!(
        "op_pi native hook received: provider={} event={} repo={} session={}",
        raw_native_field(
            &payload,
            &["/provider", "/source/provider", "/context/provider"],
        ),
        raw_native_field(
            &payload,
            &[
                "/event_name",
                "/event",
                "/hook_event_name",
                "/hookEventName",
            ],
        ),
        raw_native_field(
            &payload,
            &[
                "/repo_name",
                "/context/repo_name",
                "/project",
                "/project_name",
            ],
        ),
        raw_native_field(
            &payload,
            &[
                "/session_id",
                "/sessionId",
                "/context/session_id",
                "/event_payload/session_id",
            ],
        ),
    );

    let event = match incoming_event_from_native_hook_json(&payload) {
        Ok(event) => normalize_event(event),
        Err(error) => {
            with_native_observability(&state.native_observability, |observability| {
                observability.observe_dropped_raw(&payload, "normalization_failed");
            });
            eprintln!(
                "op_pi native hook dropped: provider={} event={} reason=normalization_failed error={}",
                raw_native_field(
                    &payload,
                    &["/provider", "/source/provider", "/context/provider"],
                ),
                raw_native_field(
                    &payload,
                    &[
                        "/event_name",
                        "/event",
                        "/hook_event_name",
                        "/hookEventName",
                    ],
                ),
                error
            );
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": error.to_string()})),
            )
                .into_response();
        }
    };

    with_native_observability(&state.native_observability, |observability| {
        observability.observe_normalized(&event);
    });

    if raw_non_git || native_hook_should_drop(&event) {
        telemetry::emit(event_record(
            telemetry::event_name::EVENT_DROPPED,
            telemetry::reason::DROP_NON_GIT_NATIVE_HOOK,
            &event,
            json!({"dropped": true, "source": "native_hook"}),
        ));
        with_native_observability(&state.native_observability, |observability| {
            observability.observe_dropped(&event, NATIVE_NON_GIT_OUTCOME);
        });
        eprintln!(
            "op_pi native hook dropped: {} reason={}",
            native_event_telemetry_fields(&event),
            NATIVE_NON_GIT_OUTCOME
        );
        return (
            StatusCode::ACCEPTED,
            Json(json!({
                "ok": true,
                "type": event.kind,
                "dropped": true,
                "reason": "non_git",
            })),
        )
            .into_response();
    }

    if let Some(defer) = stale_native_replay_defer(&event, &payload) {
        with_native_observability(&state.native_observability, |observability| {
            observability.observe_deferred(&event, defer.reason);
        });
        eprintln!(
            "op_pi native hook deferred: {} reason={} age_secs={}",
            native_event_telemetry_fields(&event),
            defer.reason,
            defer.age.as_secs()
        );
        return stale_replay_defer_response(&event.kind, &defer);
    }

    accept_event(&state, event).await
}

fn native_payload_is_non_git(payload: &Value) -> bool {
    payload
        .get(NATIVE_NORMALIZATION_OUTCOME_FIELD)
        .and_then(Value::as_str)
        == Some(NATIVE_NON_GIT_OUTCOME)
        || payload
            .get("event_payload")
            .and_then(|payload| payload.get(NATIVE_NORMALIZATION_OUTCOME_FIELD))
            .and_then(Value::as_str)
            == Some(NATIVE_NON_GIT_OUTCOME)
        || payload
            .get("payload")
            .and_then(|payload| payload.get(NATIVE_NORMALIZATION_OUTCOME_FIELD))
            .and_then(Value::as_str)
            == Some(NATIVE_NON_GIT_OUTCOME)
}

fn raw_native_field(payload: &Value, pointers: &[&str]) -> String {
    pointers
        .iter()
        .find_map(|pointer| payload.pointer(pointer).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown")
        .to_string()
}

fn native_hook_should_drop(event: &IncomingEvent) -> bool {
    if event
        .payload
        .get(NATIVE_NORMALIZATION_OUTCOME_FIELD)
        .and_then(Value::as_str)
        == Some(NATIVE_NON_GIT_OUTCOME)
    {
        return true;
    }

    event
        .payload
        .get("payload")
        .and_then(|payload| payload.get(NATIVE_NORMALIZATION_OUTCOME_FIELD))
        .and_then(Value::as_str)
        == Some(NATIVE_NON_GIT_OUTCOME)
}

#[derive(Debug, Clone)]
struct NativeReplayDefer {
    reason: &'static str,
    timestamp: String,
    age: Duration,
}

fn stale_native_replay_defer(
    event: &IncomingEvent,
    raw_payload: &Value,
) -> Option<NativeReplayDefer> {
    stale_replay_defer(
        event.canonical_kind(),
        raw_payload,
        NATIVE_REPLAY_TIMESTAMP_POINTERS,
    )
}

fn stale_replay_defer(
    kind: &str,
    raw_payload: &Value,
    timestamp_pointers: &[&str],
) -> Option<NativeReplayDefer> {
    if !is_replay_sensitive_native_kind(kind) {
        return None;
    }

    let timestamp = replay_timestamp(raw_payload, timestamp_pointers)?;
    let observed_at = parse_native_replay_timestamp(&timestamp)?;
    let now = OffsetDateTime::now_utc();
    let age = now - observed_at;
    let age = age.try_into().ok()?;

    (age > STALE_NATIVE_REPLAY_GRACE).then_some(NativeReplayDefer {
        reason: STALE_NATIVE_REPLAY_REASON,
        timestamp,
        age,
    })
}

fn is_replay_sensitive_native_kind(kind: &str) -> bool {
    matches!(
        kind,
        "tool.pre" | "tool.post" | "session.prompt-submitted" | "session.stopped"
    )
}

fn replay_timestamp(raw_payload: &Value, pointers: &[&str]) -> Option<String> {
    pointers
        .iter()
        .find_map(|pointer| timestamp_string(raw_payload.pointer(pointer)))
}

fn stale_replay_defer_response(kind: &str, defer: &NativeReplayDefer) -> axum::response::Response {
    eprintln!(
        "op_pi deferred stale replay: type={} reason={} timestamp={} age_secs={}",
        kind,
        defer.reason,
        defer.timestamp,
        defer.age.as_secs()
    );
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "ok": true,
            "type": kind,
            "deferred": true,
            "quarantined": true,
            "reason": defer.reason,
            "timestamp": defer.timestamp,
            "age_secs": defer.age.as_secs(),
        })),
    )
        .into_response()
}

fn timestamp_string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn parse_native_replay_timestamp(value: &str) -> Option<OffsetDateTime> {
    if let Ok(parsed) = OffsetDateTime::parse(value, &Rfc3339) {
        return Some(parsed);
    }

    let integer = value.trim().parse::<i64>().ok()?;
    let unix_seconds = if integer.unsigned_abs() >= 10_000_000_000 {
        integer / 1000
    } else {
        integer
    };
    OffsetDateTime::from_unix_timestamp(unix_seconds).ok()
}
async fn accept_event(state: &AppState, event: IncomingEvent) -> axum::response::Response {
    let envelope = match from_incoming_event(&event) {
        Ok(envelope) => envelope,
        Err(error) => {
            if is_native_hook_event(&event) {
                with_native_observability(&state.native_observability, |observability| {
                    observability.observe_dropped(&event, "validation_error");
                });
            }
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": error.to_string()})),
            )
                .into_response();
        }
    };

    if event.canonical_kind() == "discord.message-create" {
        if let Err(error) = handle_discord_watch(state, &event).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": error.to_string()})),
            )
                .into_response();
        }
        return local_only_event_response(&event, &envelope);
    }

    if event.canonical_kind() == "discord-watch.nudge-intent" {
        return local_only_event_response(&event, &envelope);
    }

    if let Some(handler_event) = run_matching_gajae_handler(state, &event).await {
        return enqueue_accepted_event(state, handler_event).await;
    }

    enqueue_accepted_event(state, event).await
}

async fn enqueue_accepted_event(
    state: &AppState,
    event: IncomingEvent,
) -> axum::response::Response {
    let envelope = match from_incoming_event(&event) {
        Ok(envelope) => envelope,
        Err(error) => {
            if is_native_hook_event(&event) {
                with_native_observability(&state.native_observability, |observability| {
                    observability.observe_dropped(&event, "validation_error");
                });
            }
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": error.to_string()})),
            )
                .into_response();
        }
    };

    match enqueue_event(&state.tx, event.clone()).await {
        Ok(()) => {
            expire_terminal_tmux_registration(state, &event).await;
            telemetry::emit(event_record(
                telemetry::event_name::EVENT_ACCEPTED,
                telemetry::reason::ACCEPT_ENQUEUED,
                &event,
                json!({"event_id": envelope.id.to_string()}),
            ));
            (
                StatusCode::ACCEPTED,
                Json(json!({
                    "ok": true,
                    "type": event.kind,
                    "event_id": envelope.id.to_string(),
                })),
            )
                .into_response()
        }
        Err(error) => {
            if is_native_hook_event(&event) {
                with_native_observability(&state.native_observability, |observability| {
                    observability.observe_dropped(&event, "queue_unavailable");
                });
            }
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"ok": false, "error": error.to_string()})),
            )
                .into_response()
        }
    }
}

fn local_only_event_response(
    event: &IncomingEvent,
    envelope: &crate::event::EventEnvelope,
) -> axum::response::Response {
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "ok": true,
            "type": event.kind,
            "event_id": envelope.id.to_string(),
            "local_only": true,
        })),
    )
        .into_response()
}

async fn run_matching_gajae_handler(
    state: &AppState,
    event: &IncomingEvent,
) -> Option<IncomingEvent> {
    if !state.config.gajae.handlers_enabled {
        return None;
    }
    if event.canonical_kind().starts_with("gajae.handler.") {
        return None;
    }

    let route = matching_gajae_route(&state.config, event)?;
    let action = handler_action(route.gajae.as_ref()?);
    let event_json = handler_event_json(event);
    let limits = HandlerLimits {
        timeout: Duration::from_millis(state.config.gajae.handler_timeout_ms),
        max_output_bytes: state.config.gajae.handler_max_output_bytes,
    };

    let outcome = match crate::gajae::run_handler(&action, &event_json, limits).await {
        Ok(outcome) => outcome,
        Err(error) => HandlerOutcome::Failed {
            code: None,
            stdout: String::new(),
            stderr: bounded_handler_text(&error.to_string()),
        },
    };

    Some(handler_outcome_event(event, &action, outcome))
}

fn matching_gajae_route<'a>(config: &'a AppConfig, event: &IncomingEvent) -> Option<&'a RouteRule> {
    let context = event.template_context();
    config
        .routes
        .iter()
        .filter(|route| route.gajae.is_some())
        .filter(|route| route_matches_event(route, event.canonical_kind(), &context))
        .max_by_key(|route| route_specificity(route, &context))
}

fn route_matches_event(
    route: &RouteRule,
    canonical_kind: &str,
    context: &std::collections::BTreeMap<String, String>,
) -> bool {
    route_event_candidates(canonical_kind)
        .iter()
        .any(|candidate| crate::router::glob_match(&route.event, candidate))
        && route.filter.iter().all(|(key, expected)| {
            context
                .get(key)
                .map(|actual| crate::router::glob_match(expected, actual))
                .unwrap_or(false)
        })
}

fn route_event_candidates(canonical_kind: &str) -> [&str; 2] {
    let suffix = canonical_kind
        .split_once('.')
        .map(|(_, suffix)| suffix)
        .unwrap_or(canonical_kind);
    [canonical_kind, suffix]
}

fn route_specificity(
    route: &RouteRule,
    context: &std::collections::BTreeMap<String, String>,
) -> usize {
    let path_rank = if route.filter.contains_key("worktree_path")
        && context
            .get("worktree_path")
            .is_some_and(|value| !value.trim().is_empty())
    {
        3
    } else if route.filter.contains_key("repo_path")
        && context
            .get("repo_path")
            .is_some_and(|value| !value.trim().is_empty())
    {
        2
    } else if route.filter.contains_key("repo_name")
        && context
            .get("repo_name")
            .is_some_and(|value| !value.trim().is_empty())
    {
        1
    } else {
        0
    };

    (path_rank * 100) + route.filter.len()
}

fn handler_action(config: &GajaeRouteAction) -> HandlerAction {
    HandlerAction {
        subcommand: config.subcommand.clone(),
        args: config.args.clone(),
        requires_approval: config.requires_approval,
    }
}

fn handler_event_json(event: &IncomingEvent) -> Value {
    json!({
        "type": event.canonical_kind(),
        "payload": event.payload,
        "channel": event.channel,
        "mention": event.mention,
        "format": event.format.as_ref().map(|format| format.as_str()),
        "template": event.template,
    })
}

fn handler_outcome_event(
    source: &IncomingEvent,
    action: &HandlerAction,
    outcome: HandlerOutcome,
) -> IncomingEvent {
    let (kind, payload) = match outcome {
        HandlerOutcome::Completed(output) => (
            "gajae.handler.completed",
            json!({
                "source_event": source.canonical_kind(),
                "subcommand": action.subcommand,
                "output": output,
            }),
        ),
        HandlerOutcome::ApprovalRequired(output) => (
            "gajae.handler.approval-required",
            json!({
                "source_event": source.canonical_kind(),
                "subcommand": action.subcommand,
                "output": output,
                "approval_required": true,
            }),
        ),
        HandlerOutcome::Failed {
            code,
            stdout,
            stderr,
        } => (
            "gajae.handler.failed",
            json!({
                "source_event": source.canonical_kind(),
                "subcommand": action.subcommand,
                "exit_code": code,
                "stdout": bounded_handler_text(&stdout),
                "stderr": bounded_handler_text(&stderr),
            }),
        ),
        HandlerOutcome::TimedOut => (
            "gajae.handler.timeout",
            json!({
                "source_event": source.canonical_kind(),
                "subcommand": action.subcommand,
                "timeout": true,
            }),
        ),
    };

    IncomingEvent {
        kind: kind.to_string(),
        channel: source.channel.clone(),
        mention: None,
        format: Some(MessageFormat::Compact),
        template: None,
        payload,
    }
}

fn bounded_handler_text(value: &str) -> String {
    value.chars().take(512).collect()
}

async fn handle_discord_watch(state: &AppState, event: &IncomingEvent) -> Result<()> {
    let now_ms = OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000;
    let _guard = state.discord_watch_lock.lock().await;
    crate::discord_watch::handle_local_intent_event(
        &state.config.discord_watch,
        &state.cron_state_path,
        event,
        now_ms as i64,
    )?;
    Ok(())
}

async fn expire_terminal_tmux_registration(state: &AppState, event: &IncomingEvent) {
    if !is_terminal_session_event(event.canonical_kind()) {
        return;
    }

    let candidates = terminal_session_candidates(&event.payload);
    if candidates.is_empty() {
        return;
    }

    let mut registry = state.tmux_registry.write().await;
    for session in candidates {
        if registry.remove(&session).is_some() {
            telemetry::emit(tmux_terminal_expiry_record(&session));
        }
    }
}

fn is_terminal_session_event(kind: &str) -> bool {
    matches!(
        kind,
        "session.finished" | "session.stopped" | "session.pr-created"
    )
}

fn tmux_terminal_expiry_record(session: &str) -> serde_json::Map<String, Value> {
    let mut record = telemetry::record(
        telemetry::event_name::SOURCE_INVENTORY,
        "terminal_session_expired",
        format!("source:tmux:{session}"),
    );
    record.insert("source".to_string(), json!("tmux"));
    record.insert("session".to_string(), json!(session));
    record
}

fn terminal_session_candidates(payload: &Value) -> Vec<String> {
    let mut candidates = Vec::new();
    for key in ["session", "session_name", "session_id", "agent_name"] {
        if let Some(value) = payload.get(key).and_then(Value::as_str) {
            let value = value.trim();
            if !value.is_empty() && !candidates.iter().any(|candidate| candidate == value) {
                candidates.push(value.to_string());
            }
        }
    }
    candidates
}

async fn register_tmux(
    State(state): State<AppState>,
    Json(registration): Json<RegisteredTmuxSession>,
) -> impl IntoResponse {
    state
        .tmux_registry
        .write()
        .await
        .insert(registration.session.clone(), registration.clone());
    (
        StatusCode::ACCEPTED,
        Json(json!({"ok": true, "session": registration.session})),
    )
        .into_response()
}

async fn list_tmux(State(state): State<AppState>) -> impl IntoResponse {
    match list_active_tmux_registrations(state.config.as_ref(), &state.tmux_registry).await {
        Ok(registrations) => (StatusCode::OK, Json(json!(registrations))).into_response(),
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ok": false, "error": error.to_string()})),
        )
            .into_response(),
    }
}

fn gajae_hold_target(config: &AppConfig, repo: &str) -> Option<String> {
    config
        .routes
        .iter()
        .filter(|route| {
            matches!(
                route.event.as_str(),
                "gajae.release.hold" | "gajae.merge.hold" | "gajae.*"
            )
        })
        .find(|route| {
            route.filter.is_empty()
                || route
                    .filter
                    .get("repo")
                    .or_else(|| route.filter.get("repo_name"))
                    .is_some_and(|expected| crate::router::glob_match(expected, repo))
        })
        .and_then(|route| route.channel.clone().or_else(|| route.thread.clone()))
        .or_else(|| config.gajae.hold_target_channel.clone())
}

async fn post_linear(
    State(state): State<AppState>,
    Extension(linear): Extension<LinearIntake>,
    Extension(ledger): Extension<Option<evidence_ledger::EvidenceLedger>>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> axum::response::Response {
    let Some(secret) = state.config.linear.webhook_secret.as_deref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let now_ms = (linear.now_ms)();
    if !linear
        .rate
        .lock()
        .expect("Linear rate limiter poisoned")
        .try_consume(now_ms)
    {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let Ok(_permit) = linear.permits.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let body = match tokio::time::timeout(
        linear.read_timeout,
        axum::body::to_bytes(body, crate::intake::LINEAR_MAX_BODY_BYTES),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(error)) if is_linear_body_limit_error(&error) => {
            return StatusCode::PAYLOAD_TOO_LARGE.into_response();
        }
        Ok(Err(_)) | Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let signature = headers
        .get("linear-signature")
        .and_then(|value| value.to_str().ok());
    if crate::intake::verify_linear_signature(&body, signature, secret).is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(_signature) = signature else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let delivery = headers.get("linear-delivery").and_then(|value| {
        (value.as_bytes().len() <= LINEAR_DELIVERY_MAX_BYTES)
            .then(|| std::str::from_utf8(value.as_bytes()).ok())
            .flatten()
    });
    if headers
        .get("linear-delivery")
        .is_some_and(|value| value.as_bytes().len() > LINEAR_DELIVERY_MAX_BYTES)
    {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let mut event =
        match crate::intake::normalize_linear_webhook(&body, delivery, (linear.now_ms)()) {
            Ok(event) => event,
            Err(crate::intake::IntakeError::Unauthorized(_)) => {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            Err(crate::intake::IntakeError::BadRequest(_)) => {
                return StatusCode::BAD_REQUEST.into_response();
            }
        };
    let event_id = uuid::Uuid::new_v4().to_string();
    if let Some(payload) = event.payload.as_object_mut() {
        payload.insert("event_id".to_string(), json!(event_id));
        payload.insert("correlation_id".to_string(), json!(event_id));
    }
    let timestamp = event.payload["webhook"]["webhookTimestamp"]
        .as_u64()
        .unwrap_or_default();
    let expires_at = timestamp.saturating_add(LINEAR_REPLAY_TTL_MS);
    let event_kind = event.canonical_kind().to_string();
    if let Some(ledger) = ledger {
        // The durable dedupe key must be stable across Linear redeliveries, so it
        // is the digest of the signed body, not the per-request correlation uuid.
        let dedupe_key = hex::encode(LinearIntake::replay_identity(&body));
        let record = evidence_ledger::InboxRecord {
            event_id: dedupe_key,
            kind: event_kind.clone(),
            payload: event.payload.clone(),
        };
        let tx = state.tx.clone();
        let signal = move || async move {
            tx.try_send(event).map_err(|_| {
                Box::<dyn std::error::Error + Send + Sync>::from("linear queue send failed")
            })
        };
        return match ledger.accept_and_signal(&record, &[], signal).await {
            Ok(outcome) => {
                if matches!(outcome, evidence_ledger::AppendOutcome::Committed) {
                    telemetry::emit(linear_accepted_record(&event_kind, &event_id));
                }
                StatusCode::OK.into_response()
            }
            Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
    }
    let accepted_record = linear_accepted_record(&event_kind, &event_id);
    let identity = LinearIntake::replay_identity(&body);
    let accepted = {
        let mut replay = linear.replay.lock().expect("Linear replay cache poisoned");
        replay.prune(now_ms);
        if replay.contains(&identity) {
            return StatusCode::OK.into_response();
        }
        if replay.entries.len() == LINEAR_REPLAY_CAPACITY {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        if state.tx.try_send(event).is_err() {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        replay.insert(identity, expires_at);
        true
    };
    if accepted {
        telemetry::emit(accepted_record);
    }
    StatusCode::OK.into_response()
}

fn is_linear_body_limit_error(error: &axum::Error) -> bool {
    let mut source: &(dyn std::error::Error + 'static) = error;
    loop {
        if source.is::<http_body_util::LengthLimitError>() {
            return true;
        }
        let Some(next) = source.source() else {
            return false;
        };
        source = next;
    }
}

fn linear_accepted_record(event_kind: &str, event_id: &str) -> serde_json::Map<String, Value> {
    let mut record = telemetry::record(
        telemetry::event_name::EVENT_ACCEPTED,
        telemetry::reason::ACCEPT_ENQUEUED,
        event_id,
    );
    record.insert("event_kind".to_string(), json!(event_kind));
    record.insert("details".to_string(), json!({"event_id": event_id}));
    record
}

fn unix_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

async fn post_aws_sns(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let topic_arn = payload
        .get("TopicArn")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !crate::intake::topic_allowed(&state.config.aws.topic_allowlist, topic_arn) {
        return (StatusCode::FORBIDDEN, "SNS topic is not allowlisted").into_response();
    }
    if let Err(error) = crate::intake::verify_sns_signature(&payload, &state.sns_cert_cache).await {
        return (StatusCode::FORBIDDEN, error.to_string()).into_response();
    }
    match crate::intake::normalize_sns_envelope(&payload) {
        Ok(event) => enqueue_accepted_event(&state, normalize_event(event)).await,
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

async fn post_aws_eventbridge(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    if state.config.aws.webhook_secret.is_none() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "aws.eventbridge intake is not configured; set [aws].webhook_secret",
        )
            .into_response();
    }
    if let Err(error) = crate::intake::verify_secret(
        headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok()),
        state.config.aws.webhook_secret.as_deref(),
    ) {
        return (StatusCode::UNAUTHORIZED, error.to_string()).into_response();
    }
    match crate::intake::normalize_eventbridge(&payload) {
        Ok(event) => enqueue_accepted_event(&state, normalize_event(event)).await,
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

async fn post_cloudflare_notification(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    if state.config.cloudflare.webhook_secret.is_none() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "cloudflare notification intake is not configured; set [cloudflare].webhook_secret",
        )
            .into_response();
    }
    if let Err(error) = crate::intake::verify_secret(
        headers
            .get("cf-webhook-auth")
            .and_then(|value| value.to_str().ok()),
        state.config.cloudflare.webhook_secret.as_deref(),
    ) {
        return (StatusCode::UNAUTHORIZED, error.to_string()).into_response();
    }
    match crate::intake::normalize_cf_notification(&payload) {
        Ok(event) => enqueue_accepted_event(&state, normalize_event(event)).await,
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

async fn post_cloudflare_logpush(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::BTreeMap<String, String>>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    if state.config.cloudflare.logpush_secret.is_none() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "cloudflare logpush intake is not configured; set [cloudflare].logpush_secret",
        )
            .into_response();
    }
    if let Err(error) = crate::intake::verify_secret(
        headers
            .get("x-logpush-secret")
            .and_then(|value| value.to_str().ok()),
        state.config.cloudflare.logpush_secret.as_deref(),
    ) {
        return (StatusCode::UNAUTHORIZED, error.to_string()).into_response();
    }
    let dataset = params
        .get("dataset")
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());
    let content_encoding = headers
        .get("content-encoding")
        .and_then(|value| value.to_str().ok());
    match crate::intake::normalize_cf_logpush_batch(&dataset, &body, content_encoding) {
        Ok(event) => enqueue_accepted_event(&state, normalize_event(event)).await,
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

#[cfg(test)]
async fn post_google_calendar(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> axum::response::Response {
    process_google_calendar_notification(&state, &headers, None).await
}

async fn post_google_calendar_with_source(
    State(state): State<AppState>,
    Extension(notifications): Extension<mpsc::Sender<CalendarNotification>>,
    headers: HeaderMap,
) -> axum::response::Response {
    process_google_calendar_notification(&state, &headers, Some(&notifications)).await
}

async fn process_google_calendar_notification(
    state: &AppState,
    headers: &HeaderMap,
    notifications: Option<&mpsc::Sender<CalendarNotification>>,
) -> axum::response::Response {
    let Some(expected_token) = state
        .config
        .google_calendar
        .channel_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
    else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Google Calendar intake is not configured; set [google_calendar].channel_token",
        )
            .into_response();
    };
    let provided_token = headers
        .get("x-goog-channel-token")
        .and_then(|value| value.to_str().ok());
    if let Err(error) = crate::intake::verify_secret(provided_token, Some(expected_token)) {
        return (StatusCode::UNAUTHORIZED, error.to_string()).into_response();
    }
    if !state
        .calendar_webhook_rate_limit
        .lock()
        .await
        .try_consume(CALENDAR_WEBHOOK_RATE_LIMIT_KEY)
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Google Calendar notification rate limit exceeded",
        )
            .into_response();
    }

    let channel_id = match google_calendar_header(headers, "x-goog-channel-id") {
        Ok(value) => value,
        Err(error) => return (StatusCode::BAD_REQUEST, error).into_response(),
    };
    let resource_id = match google_calendar_header(headers, "x-goog-resource-id") {
        Ok(value) => value,
        Err(error) => return (StatusCode::BAD_REQUEST, error).into_response(),
    };
    let resource_uri = match google_calendar_header(headers, "x-goog-resource-uri") {
        Ok(value) => value,
        Err(error) => return (StatusCode::BAD_REQUEST, error).into_response(),
    };
    let resource_state = match google_calendar_header(headers, "x-goog-resource-state") {
        Ok(value) => value,
        Err(error) => return (StatusCode::BAD_REQUEST, error).into_response(),
    };
    let message_number = match google_calendar_header(headers, "x-goog-message-number") {
        Ok(value) => value,
        Err(error) => return (StatusCode::BAD_REQUEST, error).into_response(),
    };

    match crate::intake::normalize_google_calendar_notification(
        channel_id,
        resource_id,
        resource_uri,
        resource_state,
        message_number,
        headers
            .get("x-goog-channel-expiration")
            .and_then(|value| value.to_str().ok()),
    ) {
        Ok(event) => {
            if !state.config.google_calendar.sync_enabled() {
                return enqueue_accepted_event(state, normalize_event(event)).await;
            }
            let Some(notifications) = notifications else {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Google Calendar synchronization is temporarily unavailable",
                )
                    .into_response();
            };
            let Some(parsed_message_number) =
                event.payload.get("message_number").and_then(Value::as_u64)
            else {
                return (
                    StatusCode::BAD_REQUEST,
                    "invalid Google Calendar message number",
                )
                    .into_response();
            };
            let deferred = DeferredCalendarNotification {
                channel_id: channel_id.to_string(),
                resource_id: resource_id.to_string(),
                resource_uri: resource_uri.to_string(),
                message_number: parsed_message_number,
                resource_state: resource_state.to_string(),
            };
            let (persisted, persisted_ack) = oneshot::channel();
            let notification = CalendarNotification {
                channel_id: channel_id.to_string(),
                resource_id: resource_id.to_string(),
                resource_uri: resource_uri.to_string(),
                message_number: parsed_message_number,
                resource_state: resource_state.to_string(),
                persisted,
            };
            match tokio::time::timeout(
                CALENDAR_PERSIST_ACK_TIMEOUT,
                notifications.send(notification),
            )
            .await
            {
                Ok(Ok(())) => {
                    match tokio::time::timeout(CALENDAR_PERSIST_ACK_TIMEOUT, persisted_ack).await {
                        Ok(Ok(true)) => enqueue_accepted_event(state, normalize_event(event)).await,
                        Ok(Ok(false)) => google_calendar_acknowledgement("untracked_or_replayed"),
                        Ok(Err(_)) | Err(_) => {
                            persist_deferred_calendar_notification(state.config.clone(), deferred)
                                .await
                        }
                    }
                }
                Ok(Err(_)) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Google Calendar synchronization is temporarily unavailable",
                )
                    .into_response(),
                Err(_) => {
                    persist_deferred_calendar_notification(state.config.clone(), deferred).await
                }
            }
        }
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

async fn persist_deferred_calendar_notification(
    config: Arc<AppConfig>,
    deferred: DeferredCalendarNotification,
) -> axum::response::Response {
    match tokio::task::spawn_blocking(move || {
        persist_deferred_notification(config.as_ref(), deferred)
    })
    .await
    {
        Ok(Ok(())) => google_calendar_acknowledgement("deferred"),
        Ok(Err(_)) | Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Google Calendar synchronization is temporarily unavailable",
        )
            .into_response(),
    }
}

fn google_calendar_acknowledgement(reason: &'static str) -> axum::response::Response {
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "ok": true,
            "type": "google.calendar.changed",
            "trusted": false,
            "reason": reason,
        })),
    )
        .into_response()
}

fn google_calendar_header<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
) -> std::result::Result<&'a str, String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("missing Google Calendar header `{name}`"))
}

async fn post_github(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let event_name = headers
        .get("x-github-event")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let event = match event_name {
        "issues" if action == "opened" => {
            Some(normalize_event(IncomingEvent::github_issue_opened(
                payload
                    .pointer("/repository/full_name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown/unknown")
                    .to_string(),
                payload
                    .pointer("/issue/number")
                    .and_then(Value::as_u64)
                    .unwrap_or_default(),
                payload
                    .pointer("/issue/title")
                    .and_then(Value::as_str)
                    .unwrap_or("Untitled issue")
                    .to_string(),
                None,
            )))
        }
        "release" if matches!(action, "published" | "released" | "prereleased" | "edited") => {
            let repo = payload
                .pointer("/repository/full_name")
                .and_then(Value::as_str)
                .unwrap_or("unknown/unknown")
                .to_string();
            let tag = payload
                .pointer("/release/tag_name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let name = payload
                .pointer("/release/name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let is_prerelease = payload
                .pointer("/release/prerelease")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let url = payload
                .pointer("/release/html_url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let actor = payload
                .pointer("/sender/login")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            if let Some(target) = gajae_hold_target(&state.config, &repo) {
                return enqueue_accepted_event(
                    &state,
                    IncomingEvent::gajae_release_hold(
                        repo,
                        target,
                        action.to_string(),
                        tag,
                        format!("publish or retag release {action}"),
                        "release and retag boundaries require owner/maintainer approval; autonomous execution cannot create, edit, publish, or retag releases".to_string(),
                        actor,
                    ),
                )
                .await;
            }

            Some(normalize_event(IncomingEvent::github_release(
                action,
                repo,
                tag,
                name,
                is_prerelease,
                url,
                actor,
                None,
            )))
        }
        "pull_request" => {
            let repo = payload
                .pointer("/repository/full_name")
                .and_then(Value::as_str)
                .unwrap_or("unknown/unknown")
                .to_string();
            let number = payload
                .pointer("/pull_request/number")
                .or_else(|| payload.pointer("/number"))
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let title = payload
                .pointer("/pull_request/title")
                .and_then(Value::as_str)
                .unwrap_or("Untitled pull request")
                .to_string();
            let url = payload
                .pointer("/pull_request/html_url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let merged = payload
                .pointer("/pull_request/merged")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let base_ref = payload
                .pointer("/pull_request/base/ref")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let merge_sha = payload
                .pointer("/pull_request/merge_commit_sha")
                .and_then(Value::as_str)
                .or_else(|| {
                    payload
                        .pointer("/pull_request/head/sha")
                        .and_then(Value::as_str)
                })
                .unwrap_or_default()
                .to_string();
            let actor = payload
                .pointer("/sender/login")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            if action == "closed"
                && merged
                && matches!(base_ref, "main" | "master")
                && let Some(target) = gajae_hold_target(&state.config, &repo)
            {
                return enqueue_accepted_event(
                    &state,
                    IncomingEvent::gajae_merge_hold(
                        repo,
                        target,
                        "merge-to-main".to_string(),
                        merge_sha,
                        format!("merge pull request #{number} into {base_ref}"),
                        "main branch merge boundaries require owner/maintainer approval; autonomous execution cannot merge directly to main".to_string(),
                        actor,
                    ),
                )
                .await;
            }
            match action {
                "opened" => Some(normalize_event(IncomingEvent::github_pr_status_changed(
                    repo,
                    number,
                    title,
                    "unknown".to_string(),
                    "opened".to_string(),
                    url,
                    None,
                ))),
                "closed" => Some(normalize_event(IncomingEvent::github_pr_status_changed(
                    repo,
                    number,
                    title,
                    "open".to_string(),
                    "closed".to_string(),
                    url,
                    None,
                ))),
                _ => None,
            }
        }
        _ => None,
    };

    let Some(event) = event else {
        let reason = if event_name == "pull_request" {
            "unsupported pull_request action"
        } else {
            "unsupported event"
        };
        return (
            StatusCode::ACCEPTED,
            Json(json!({"ok": true, "ignored": true, "reason": reason})),
        )
            .into_response();
    };

    if let Err(error) = from_incoming_event(&event) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": error.to_string()})),
        )
            .into_response();
    }

    match enqueue_event(&state.tx, event).await {
        Ok(()) => (StatusCode::ACCEPTED, Json(json!({"ok": true}))).into_response(),
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ok": false, "error": error.to_string()})),
        )
            .into_response(),
    }
}

async fn update_status(State(state): State<AppState>) -> impl IntoResponse {
    let pending = state.pending_update.read().await;
    match pending.as_ref() {
        Some(update) => (
            StatusCode::OK,
            Json(json!({
                "pending": true,
                "current_version": update.current_version,
                "latest_version": update.latest_version,
                "release_url": update.release_url,
                "detected_at": update.detected_at,
            })),
        )
            .into_response(),
        None => (
            StatusCode::OK,
            Json(json!({
                "pending": false,
                "current_version": VERSION,
            })),
        )
            .into_response(),
    }
}

async fn approve_update(State(state): State<AppState>) -> impl IntoResponse {
    match update::approve_update(&state.pending_update, &state.config, &state.tx).await {
        Ok(update) => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "updated_to": update.latest_version,
            })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": error.to_string()})),
        )
            .into_response(),
    }
}

async fn dismiss_update(State(state): State<AppState>) -> impl IntoResponse {
    match update::dismiss_update(&state.pending_update).await {
        Ok(update) => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "dismissed_version": update.latest_version,
            })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": error.to_string()})),
        )
            .into_response(),
    }
}

async fn enqueue_event(tx: &mpsc::Sender<IncomingEvent>, event: IncomingEvent) -> Result<()> {
    tx.send(event)
        .await
        .map_err(|error| format!("event queue unavailable: {error}").into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::config::{CronJob, CronJobKind};
    use crate::events::{MessageFormat, RoutingMetadata};
    use crate::router::Router;
    use crate::sink::SinkTarget;
    use crate::source::tmux::{ParentProcessInfo, RegistrationSource};
    use axum::body::{Body, to_bytes};
    use axum::http::{HeaderValue, Request};
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tempfile::tempdir;
    use tokio::time::{Duration, timeout};
    use tower::ServiceExt;

    fn native_hook_test_state() -> (AppState, mpsc::Receiver<IncomingEvent>) {
        let (tx, rx) = mpsc::channel(8);
        (
            AppState {
                config: Arc::new(AppConfig::default()),
                port: 25294,
                tx,
                tmux_registry: Arc::new(RwLock::new(HashMap::new())),
                pending_update: update::new_shared_pending_update(),
                native_observability: new_shared_native_hook_observability(),
                cron_state_path: PathBuf::from("cron-state.json"),
                discord_watch_lock: Arc::new(Mutex::new(())),
                sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
                calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                    CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                    CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
                ))),
                source_health: new_shared_source_health(),
            },
            rx,
        )
    }

    fn app_state_with_config(config: AppConfig) -> (AppState, mpsc::Receiver<IncomingEvent>) {
        app_state_with_config_and_capacity(config, 8)
    }

    fn app_state_with_config_and_capacity(
        config: AppConfig,
        capacity: usize,
    ) -> (AppState, mpsc::Receiver<IncomingEvent>) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            AppState {
                config: Arc::new(config),
                port: 25294,
                tx,
                tmux_registry: Arc::new(RwLock::new(HashMap::new())),
                pending_update: update::new_shared_pending_update(),
                native_observability: new_shared_native_hook_observability(),
                source_health: new_shared_source_health(),
                cron_state_path: PathBuf::from("cron-state.json"),
                sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
                calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                    CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                    CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
                ))),
                discord_watch_lock: Arc::new(Mutex::new(())),
            },
            rx,
        )
    }

    fn hold_config() -> AppConfig {
        AppConfig {
            gajae: crate::config::GajaeConfig {
                hold_target_channel: Some("owner-maintainer".into()),
                ..crate::config::GajaeConfig::default()
            },
            defaults: crate::config::DefaultsConfig {
                channel: Some("general-zero-backlog".into()),
                ..crate::config::DefaultsConfig::default()
            },
            routes: vec![RouteRule {
                event: "gajae.*".into(),
                filter: std::collections::BTreeMap::from([(
                    "repo".into(),
                    "IYENTeam/op_pi".into(),
                )]),
                channel: Some("owner-maintainer".into()),
                ..RouteRule::default()
            }],
            ..AppConfig::default()
        }
    }

    fn tmux_registration(session: &str) -> RegisteredTmuxSession {
        RegisteredTmuxSession {
            session: session.into(),
            channel: Some("alerts".into()),
            mention: Some("<@123>".into()),
            routing: RoutingMetadata::default(),
            keywords: vec!["error".into()],
            keyword_window_secs: 30,
            stale_minutes: 15,
            format: None,
            registered_at: "2026-04-02T00:00:00Z".into(),
            registration_source: RegistrationSource::CliWatch,
            parent_process: Some(ParentProcessInfo {
                pid: 4242,
                name: Some("codex".into()),
            }),
            active_wrapper_monitor: true,
        }
    }

    fn gajae_test_event() -> IncomingEvent {
        IncomingEvent {
            kind: "github.pr.opened".into(),
            channel: Some("ops".into()),
            mention: None,
            format: None,
            template: None,
            payload: json!({"repo": "op_pi", "number": 250}),
        }
    }

    #[test]
    fn gajae_handler_completed_event_is_typed_and_bounded_to_data_output() {
        let action = HandlerAction {
            subcommand: "handle-event".into(),
            args: Vec::new(),
            requires_approval: false,
        };
        let event = handler_outcome_event(
            &gajae_test_event(),
            &action,
            HandlerOutcome::Completed(json!({"summary": "ok"})),
        );

        assert_eq!(event.kind, "gajae.handler.completed");
        assert_eq!(event.payload["source_event"], json!("github.pr.opened"));
        assert_eq!(event.payload["output"]["summary"], json!("ok"));
    }

    #[test]
    fn gajae_handler_timeout_event_is_bounded() {
        let action = HandlerAction {
            subcommand: "handle-event".into(),
            args: vec!["--profile".into(), "safe".into()],
            requires_approval: false,
        };
        let event = handler_outcome_event(&gajae_test_event(), &action, HandlerOutcome::TimedOut);

        assert_eq!(event.kind, "gajae.handler.timeout");
        assert_eq!(event.payload["timeout"], json!(true));
        assert!(event.payload.get("stdout").is_none());
        assert!(event.payload.get("stderr").is_none());
    }

    #[test]
    fn gajae_handler_failed_event_bounds_diagnostics_without_raw_dump() {
        let action = HandlerAction {
            subcommand: "handle-event".into(),
            args: Vec::new(),
            requires_approval: false,
        };
        let raw = "x".repeat(2_000);
        let event = handler_outcome_event(
            &gajae_test_event(),
            &action,
            HandlerOutcome::Failed {
                code: Some(17),
                stdout: raw.clone(),
                stderr: raw,
            },
        );

        assert_eq!(event.kind, "gajae.handler.failed");
        assert_eq!(event.payload["exit_code"], json!(17));
        assert!(event.payload["stdout"].as_str().unwrap().len() <= 512);
        assert!(event.payload["stderr"].as_str().unwrap().len() <= 512);
    }

    #[test]
    fn gajae_handler_mutating_output_requires_approval_event() {
        let action = HandlerAction {
            subcommand: "handle-event".into(),
            args: Vec::new(),
            requires_approval: false,
        };
        let event = handler_outcome_event(
            &gajae_test_event(),
            &action,
            HandlerOutcome::ApprovalRequired(json!({"mutation_requested": true})),
        );

        assert_eq!(event.kind, "gajae.handler.approval-required");
        assert_eq!(event.payload["approval_required"], json!(true));
    }

    fn git_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let git = std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .expect("git init");
        assert!(
            git.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&git.stderr)
        );
        dir
    }

    fn stale_rfc3339() -> String {
        (OffsetDateTime::now_utc() - time::Duration::hours(1))
            .format(&Rfc3339)
            .expect("format stale timestamp")
    }

    fn fresh_rfc3339() -> String {
        OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .expect("format fresh timestamp")
    }

    fn native_payload(repo: &std::path::Path, event_name: &str) -> Value {
        json!({
            "provider": "codex",
            "event_name": event_name,
            "directory": repo,
            "cwd": repo,
            "event_payload": {
                "session_id": "sess-213",
                "tool_name": "Bash",
                "cwd": repo
            }
        })
    }

    async fn post_native_payload(payload: Value) -> (Value, mpsc::Receiver<IncomingEvent>) {
        let (state, rx) = native_hook_test_state();
        let response = post_native_hook(State(state), Json(payload))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let response_json: Value = serde_json::from_slice(&body).unwrap();
        (response_json, rx)
    }

    #[tokio::test]
    async fn github_release_retag_routes_to_gajae_hold_without_release_event() {
        let (state, mut rx) = app_state_with_config(hold_config());
        let mut headers = HeaderMap::new();
        headers.insert("x-github-event", "release".parse().unwrap());
        let payload = json!({
            "action": "edited",
            "repository": {"full_name": "IYENTeam/op_pi"},
            "release": {
                "tag_name": "v0.6.9",
                "name": "op_pi 0.6.9",
                "prerelease": false,
                "html_url": "https://github.com/IYENTeam/op_pi/releases/tag/v0.6.9"
            },
            "sender": {"login": "maintainer"}
        });

        let response = post_github(State(state), headers, Json(payload))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let queued = rx.recv().await.expect("queued hold");

        assert_eq!(queued.kind, "gajae.release.hold");
        assert_eq!(queued.channel.as_deref(), Some("owner-maintainer"));
        assert_ne!(queued.channel.as_deref(), Some("general-zero-backlog"));
        assert_eq!(queued.payload["repo"], json!("IYENTeam/op_pi"));
        assert_eq!(queued.payload["target"], json!("owner-maintainer"));
        assert_eq!(queued.payload["action"], json!("edited"));
        assert_eq!(queued.payload["version"], json!("v0.6.9"));
        assert_eq!(
            queued.payload["dedupe_key"],
            json!("IYENTeam/op_pi:owner-maintainer:edited:v0.6.9")
        );
        assert_eq!(queued.payload["autonomous_execution_allowed"], json!(false));
        assert_eq!(queued.payload["held_action_executed"], json!(false));
        assert!(
            queued.payload["disallowed_action"]
                .as_str()
                .unwrap()
                .contains("release")
        );
        assert!(
            queued.payload["why_autonomous_disallowed"]
                .as_str()
                .unwrap()
                .contains("approval")
        );
        assert!(queued.payload.get("raw").is_none());
    }

    #[tokio::test]
    async fn github_main_merge_routes_to_gajae_hold_without_merge_event() {
        let (state, mut rx) = app_state_with_config(hold_config());
        let mut headers = HeaderMap::new();
        headers.insert("x-github-event", "pull_request".parse().unwrap());
        let payload = json!({
            "action": "closed",
            "repository": {"full_name": "IYENTeam/op_pi"},
            "number": 252,
            "pull_request": {
                "number": 252,
                "title": "approval hold events",
                "html_url": "https://github.com/IYENTeam/op_pi/pull/252",
                "merged": true,
                "merge_commit_sha": "0123456789abcdef0123456789abcdef01234567",
                "base": {"ref": "main"},
                "head": {"sha": "abcdef0123456789abcdef0123456789abcdef01"}
            },
            "sender": {"login": "maintainer"}
        });

        let response = post_github(State(state), headers, Json(payload))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let queued = rx.recv().await.expect("queued hold");

        assert_eq!(queued.kind, "gajae.merge.hold");
        assert_eq!(queued.channel.as_deref(), Some("owner-maintainer"));
        assert_eq!(queued.payload["repo"], json!("IYENTeam/op_pi"));
        assert_eq!(queued.payload["target"], json!("owner-maintainer"));
        assert_eq!(queued.payload["action"], json!("merge-to-main"));
        assert_eq!(
            queued.payload["sha"],
            json!("0123456789abcdef0123456789abcdef01234567")
        );
        assert_eq!(
            queued.payload["dedupe_key"],
            json!(
                "IYENTeam/op_pi:owner-maintainer:merge-to-main:0123456789abcdef0123456789abcdef01234567"
            )
        );
        assert_eq!(queued.payload["autonomous_execution_allowed"], json!(false));
        assert_eq!(queued.payload["held_action_executed"], json!(false));
        assert!(
            queued.payload["disallowed_action"]
                .as_str()
                .unwrap()
                .contains("merge pull request #252 into main")
        );
        assert!(
            queued.payload["why_autonomous_disallowed"]
                .as_str()
                .unwrap()
                .contains("approval")
        );
        assert!(queued.payload.get("raw").is_none());
    }

    fn insert_timestamp_at_path(payload: &mut Value, path: &[&str], value: String) {
        let mut current = payload;
        for key in &path[..path.len() - 1] {
            current = current
                .as_object_mut()
                .expect("object")
                .entry((*key).to_string())
                .or_insert_with(|| json!({}));
        }
        current
            .as_object_mut()
            .expect("object")
            .insert(path[path.len() - 1].to_string(), Value::String(value));
    }

    #[tokio::test]
    async fn accepted_terminal_session_event_expires_matching_tmux_registration() {
        let (tx, mut rx) = mpsc::channel(1);
        let registry: SharedTmuxRegistry = Arc::new(RwLock::new(HashMap::new()));
        registry
            .write()
            .await
            .insert("issue-221".into(), tmux_registration("issue-221"));
        registry
            .write()
            .await
            .insert("still-active".into(), tmux_registration("still-active"));
        let state = AppState {
            config: Arc::new(AppConfig::default()),
            port: 25294,
            tx,
            tmux_registry: registry.clone(),
            pending_update: update::new_shared_pending_update(),
            native_observability: new_shared_native_hook_observability(),
            source_health: new_shared_source_health(),
            cron_state_path: PathBuf::from("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };

        let response = accept_event(
            &state,
            IncomingEvent {
                kind: "session.finished".into(),
                channel: None,
                mention: None,
                format: None,
                template: None,
                payload: json!({
                    "agent_name": "issue-221",
                    "session": "issue-221",
                    "session_id": "issue-221",
                    "status": "finished"
                }),
            },
        )
        .await;

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(
            rx.recv().await.expect("queued event").kind,
            "session.finished"
        );
        let registry = registry.read().await;
        assert!(!registry.contains_key("issue-221"));
        assert!(registry.contains_key("still-active"));
    }

    #[tokio::test]
    async fn accepted_non_terminal_session_event_preserves_tmux_registration() {
        let (tx, mut rx) = mpsc::channel(1);
        let registry: SharedTmuxRegistry = Arc::new(RwLock::new(HashMap::new()));
        registry
            .write()
            .await
            .insert("issue-221".into(), tmux_registration("issue-221"));
        let state = AppState {
            config: Arc::new(AppConfig::default()),
            port: 25294,
            tx,
            tmux_registry: registry.clone(),
            pending_update: update::new_shared_pending_update(),
            native_observability: new_shared_native_hook_observability(),
            source_health: new_shared_source_health(),
            cron_state_path: PathBuf::from("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };

        let response = accept_event(
            &state,
            IncomingEvent {
                kind: "session.blocked".into(),
                channel: None,
                mention: None,
                format: None,
                template: None,
                payload: json!({
                    "agent_name": "issue-221",
                    "session": "issue-221",
                    "status": "blocked"
                }),
            },
        )
        .await;

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(
            rx.recv().await.expect("queued event").kind,
            "session.blocked"
        );
        assert!(registry.read().await.contains_key("issue-221"));
    }

    #[test]
    fn health_payload_includes_version_and_token_source() {
        let mut config = AppConfig::default();
        config.providers.discord.bot_token = Some("config-token".into());
        config.monitors.git.repos.push(Default::default());
        config.monitors.tmux.sessions.push(Default::default());
        config.monitors.workspace.push(Default::default());
        config
            .monitors
            .discord_threads
            .push(crate::config::DiscordThreadMonitor {
                parent_channel: "1506217431465463929".into(),
                parent_channel_name: Some("task-request".into()),
                mention: "<@1486621536520769547>".into(),
                message: None,
                poll_interval_secs: Some(5),
            });

        let payload = health_payload(
            &config,
            25294,
            3,
            snapshot_shared(&new_shared_native_hook_observability()),
            json!({"github": {"status": "running", "last_heartbeat_at": OffsetDateTime::now_utc().format(&Rfc3339).expect("timestamp")}}),
        );

        assert_eq!(payload["ok"], Value::Bool(true));
        assert_eq!(payload["version"], Value::String(VERSION.to_string()));
        assert_eq!(payload["token_source"], Value::String("config".to_string()));
        assert_eq!(payload["port"], Value::from(25294));
        assert_eq!(payload["configured_git_monitors"], Value::from(1));
        assert_eq!(payload["configured_tmux_monitors"], Value::from(1));
        assert_eq!(payload["configured_workspace_monitors"], Value::from(1));
        assert_eq!(
            payload["configured_discord_thread_monitors"],
            Value::from(1)
        );
        assert_eq!(payload["registered_tmux_sessions"], Value::from(3));
        assert!(payload["native_hooks"]["totals"]["received"].is_number());
        assert_eq!(
            payload["sources"]["github"]["status"],
            Value::from("running")
        );
    }

    #[test]
    fn health_payload_marks_github_degraded_as_not_ok() {
        let mut config = AppConfig::default();
        config
            .monitors
            .git
            .repos
            .push(crate::config::GitRepoMonitor {
                emit_pr_status: true,
                ..Default::default()
            });
        let payload = health_payload(
            &config,
            25294,
            0,
            snapshot_shared(&new_shared_native_hook_observability()),
            json!({"github": {"status": "degraded", "last_error": "GitHub API 500"}}),
        );
        assert_eq!(payload["ok"], Value::Bool(false));
    }

    #[test]
    fn health_payload_marks_stale_github_heartbeat_as_not_ok() {
        let mut config = AppConfig::default();
        config.monitors.poll_interval_secs = 1;
        config
            .monitors
            .git
            .repos
            .push(crate::config::GitRepoMonitor {
                emit_pr_status: true,
                ..Default::default()
            });
        let stale = (OffsetDateTime::now_utc() - Duration::from_secs(300))
            .format(&Rfc3339)
            .expect("timestamp");
        let payload = health_payload(
            &config,
            25294,
            0,
            snapshot_shared(&new_shared_native_hook_observability()),
            json!({"github": {"status": "running", "last_heartbeat_at": stale}}),
        );
        assert_eq!(payload["ok"], Value::Bool(false));
        assert_eq!(payload["token_precedence_warning"], Value::Null);
    }

    #[test]
    fn discord_token_shadow_warning_names_env_var_without_leaking_value() {
        let warning = discord_token_shadow_warning("OP_PI_DISCORD_BOT_TOKEN");

        assert!(warning.contains("OP_PI_DISCORD_BOT_TOKEN"));
        assert!(warning.contains("token_source: env"));
        // The diagnostic must describe precedence only, never the secret value.
        assert!(!warning.to_lowercase().contains("config-token"));
        assert!(!warning.to_lowercase().contains("env-token"));
    }

    #[tokio::test]
    async fn source_failure_alert_defaults_to_alert_format_and_default_channel_routing() {
        let event =
            source_failure_alert_event("cron", "EOF while parsing a value at line 1 column 0");

        assert_eq!(event.kind, "custom");
        assert_eq!(event.channel, None);
        assert_eq!(event.format, Some(MessageFormat::Alert));
        assert_eq!(event.payload["source_name"], Value::from("cron"));
        assert_eq!(event.payload["health_status"], Value::from("degraded"));
        assert!(
            event.payload["message"]
                .as_str()
                .is_some_and(|message| message.contains("source 'cron' stopped"))
        );

        let mut config = AppConfig::default();
        config.defaults.channel = Some("default-alerts".into());
        let router = Router::new(Arc::new(config));
        let delivery = router.preview_delivery(&event).await.expect("delivery");

        assert_eq!(
            delivery.target,
            SinkTarget::DiscordChannel("default-alerts".into())
        );
    }

    #[tokio::test]
    async fn spawn_source_allows_cron_source_to_start_with_empty_state_and_emit_job_event() {
        let dir = tempdir().expect("tempdir");
        let state_path = dir.path().join("cron-state.json");
        fs::write(&state_path, "").expect("write invalid cron state");

        let mut config = AppConfig::default();
        config.defaults.channel = Some("default-alerts".into());
        config.cron.jobs.push(CronJob {
            id: "dev-followup".into(),
            schedule: "* * * * *".into(),
            timezone: "UTC".into(),
            enabled: true,
            channel: Some("ops".into()),
            mention: None,
            format: Some(MessageFormat::Alert),
            state_file: None,
            zero_backlog_suppression_ttl_secs: 60 * 60,
            kind: CronJobKind::CustomMessage {
                message: "check open PRs".into(),
            },
        });

        let (tx, mut rx) = mpsc::channel(4);
        spawn_source(
            CronSource::new(Arc::new(config.clone()), state_path),
            tx,
            new_shared_source_health(),
        );

        let event = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for cron job event")
            .expect("cron job event");

        assert_eq!(event.kind, "custom");
        assert_eq!(event.channel, Some("ops".into()));
        assert_eq!(event.format, Some(MessageFormat::Alert));
        assert_eq!(event.payload["cron_job_id"], Value::from("dev-followup"));
        assert_eq!(event.payload["cron_timezone"], Value::from("UTC"));

        let router = Router::new(Arc::new(config));
        let delivery = router.preview_delivery(&event).await.expect("delivery");
        assert_eq!(delivery.target, SinkTarget::DiscordChannel("ops".into()));

        let rendered = router
            .render_delivery(&event, &delivery, &crate::render::DefaultRenderer)
            .await
            .expect("rendered event");
        assert!(rendered.contains("check open PRs"));
    }

    #[tokio::test]
    async fn post_event_defers_stale_tool_replay_before_normalization_and_enqueue() {
        let (tx, mut rx) = mpsc::channel(1);
        let state = AppState {
            config: Arc::new(AppConfig::default()),
            port: 25294,
            tx,
            tmux_registry: Arc::new(RwLock::new(HashMap::new())),
            pending_update: update::new_shared_pending_update(),
            native_observability: new_shared_native_hook_observability(),
            source_health: new_shared_source_health(),
            cron_state_path: PathBuf::from("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };
        let event = IncomingEvent {
            kind: "tool.post".into(),
            channel: None,
            mention: None,
            format: None,
            template: None,
            payload: json!({
                "first_seen_at": stale_rfc3339(),
                "tool": "codex",
                "summary": "old replay"
            }),
        };

        let response = post_event(State(state), Json(event)).await.into_response();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let response_json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(response_json["ok"], json!(true));
        assert_eq!(response_json["type"], json!("tool.post"));
        assert_eq!(response_json["deferred"], json!(true));
        assert_eq!(response_json["quarantined"], json!(true));
        assert_eq!(response_json["reason"], json!(STALE_NATIVE_REPLAY_REASON));
        assert!(rx.try_recv().is_err(), "stale replay should not enqueue");
    }

    #[tokio::test]
    async fn post_event_preserves_fresh_tool_payload_with_first_seen_at() {
        let (tx, mut rx) = mpsc::channel(1);
        let state = AppState {
            config: Arc::new(AppConfig::default()),
            port: 25294,
            tx,
            tmux_registry: Arc::new(RwLock::new(HashMap::new())),
            pending_update: update::new_shared_pending_update(),
            native_observability: new_shared_native_hook_observability(),
            source_health: new_shared_source_health(),
            cron_state_path: PathBuf::from("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };
        let event = IncomingEvent {
            kind: "tool.post".into(),
            channel: None,
            mention: None,
            format: None,
            template: None,
            payload: json!({
                "first_seen_at": fresh_rfc3339(),
                "tool": "codex",
                "summary": "fresh"
            }),
        };

        let response = post_event(State(state), Json(event)).await.into_response();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let queued = rx.recv().await.expect("queued event");
        assert_eq!(queued.kind, "tool.post");
    }

    #[tokio::test]
    async fn post_event_returns_event_id_and_preserves_normalized_metadata() {
        let (tx, mut rx) = mpsc::channel(1);
        let state = AppState {
            config: Arc::new(AppConfig::default()),
            port: 25294,
            tx,
            tmux_registry: Arc::new(RwLock::new(HashMap::new())),
            pending_update: update::new_shared_pending_update(),
            native_observability: new_shared_native_hook_observability(),
            source_health: new_shared_source_health(),
            cron_state_path: PathBuf::from("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };
        let event = IncomingEvent::agent_started(
            "worker-1".into(),
            Some("sess-123".into()),
            Some("my-repo".into()),
            None,
            Some("booted".into()),
            None,
            None,
        );

        let response = post_event(State(state), Json(event)).await.into_response();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let response_json: Value = serde_json::from_slice(&body).unwrap();
        let event_id = response_json["event_id"].as_str().unwrap();
        assert!(!event_id.is_empty());
        assert_eq!(response_json["type"], Value::from("agent.started"));

        let queued = rx.recv().await.unwrap();
        assert_eq!(queued.payload["event_id"], Value::from(event_id));
        assert_eq!(queued.payload["correlation_id"], Value::from("sess-123"));
        assert!(
            queued
                .payload
                .get("first_seen_at")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
        );
    }

    #[tokio::test]
    async fn discord_watch_nudge_intent_ingress_is_local_only_without_enqueueing() {
        let (tx, mut rx) = mpsc::channel(1);
        let state = AppState {
            config: Arc::new(AppConfig::default()),
            port: 25294,
            tx,
            tmux_registry: Arc::new(RwLock::new(HashMap::new())),
            pending_update: update::new_shared_pending_update(),
            native_observability: new_shared_native_hook_observability(),
            source_health: new_shared_source_health(),
            cron_state_path: PathBuf::from("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };

        let response = accept_event(
            &state,
            IncomingEvent {
                kind: "discord-watch.nudge-intent".into(),
                channel: Some("must-not-route".into()),
                mention: None,
                format: None,
                template: None,
                payload: json!({
                    "id": "intent-1",
                    "created_at_ms": 1000,
                    "reasons": ["t3-channel-backlog"],
                    "source_channel_id": "fixture-general",
                    "source_channel_name": "general",
                    "nudge_target_channel_id": "fixture-nudge-target",
                    "content": "UltraWorkers: <#fixture-general> / general 스윕하라.",
                    "local_only": true
                }),
            },
        )
        .await;

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert!(
            timeout(Duration::from_millis(25), rx.recv()).await.is_err(),
            "local nudge intents must not enter generic Discord dispatch routing"
        );
    }

    #[tokio::test]
    async fn discord_watch_message_create_writes_local_intent_without_enqueueing() {
        let (tx, mut rx) = mpsc::channel(1);
        let dir = tempdir().expect("tempdir");
        let intents = dir.path().join("discord-watch-intents.jsonl");
        let mut config = AppConfig::default();
        config.discord_watch.enabled = true;
        config.discord_watch.gaebal_gajae_user_id = "fixture-gaebal".into();
        config.discord_watch.watched_channels = vec![crate::config::DiscordWatchChannel {
            id: "fixture-general".into(),
            name: "general".into(),
        }];
        config.discord_watch.owner_user_ids = vec!["owner".into()];
        config.discord_watch.state_file = Some(dir.path().join("discord-watch-state.json"));
        config.discord_watch.intent_file = Some(intents.clone());
        let state = AppState {
            config: Arc::new(config),
            port: 25294,
            tx,
            tmux_registry: Arc::new(RwLock::new(HashMap::new())),
            pending_update: update::new_shared_pending_update(),
            native_observability: new_shared_native_hook_observability(),
            source_health: new_shared_source_health(),
            cron_state_path: dir.path().join("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };

        let response = accept_event(
            &state,
            IncomingEvent {
                kind: "discord.message-create".into(),
                channel: Some("dm".into()),
                mention: None,
                format: None,
                template: None,
                payload: json!({
                    "message_id": "dm1",
                    "channel_id": "dm",
                    "channel_name": "owner-dm",
                    "author_id": "owner",
                    "content": "please sweep",
                    "mentions": [],
                    "direct_message": true,
                    "author_is_owner": false,
                    "timestamp_ms": 1000
                }),
            },
        )
        .await;

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert!(
            timeout(Duration::from_millis(25), rx.recv()).await.is_err(),
            "discord watch ingress must not enqueue for live dispatch"
        );
        let jsonl = fs::read_to_string(intents).expect("intent jsonl");
        assert!(
            jsonl.contains("\"local_only\":true"),
            "intent must be persisted as local-only JSONL"
        );
    }

    #[tokio::test]
    async fn discord_watch_local_intent_write_failure_rejects_without_enqueueing() {
        let (tx, mut rx) = mpsc::channel(1);
        let dir = tempdir().expect("tempdir");
        let mut config = AppConfig::default();
        config.discord_watch.enabled = true;
        config.discord_watch.gaebal_gajae_user_id = "fixture-gaebal".into();
        config.discord_watch.watched_channels = vec![crate::config::DiscordWatchChannel {
            id: "fixture-general".into(),
            name: "general".into(),
        }];
        config.discord_watch.owner_user_ids = vec!["owner".into()];
        config.discord_watch.state_file = Some(dir.path().join("discord-watch-state.json"));
        config.discord_watch.intent_file = Some(dir.path().to_path_buf());
        let state = AppState {
            config: Arc::new(config),
            port: 25294,
            tx,
            tmux_registry: Arc::new(RwLock::new(HashMap::new())),
            pending_update: update::new_shared_pending_update(),
            native_observability: new_shared_native_hook_observability(),
            source_health: new_shared_source_health(),
            cron_state_path: dir.path().join("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };

        let response = accept_event(
            &state,
            IncomingEvent {
                kind: "discord.message-create".into(),
                channel: Some("dm".into()),
                mention: None,
                format: None,
                template: None,
                payload: json!({
                    "message_id": "dm1",
                    "channel_id": "dm",
                    "channel_name": "owner-dm",
                    "author_id": "owner",
                    "content": "please sweep",
                    "mentions": [],
                    "direct_message": true,
                    "author_is_owner": false,
                    "timestamp_ms": 1000
                }),
            },
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            timeout(Duration::from_millis(25), rx.recv()).await.is_err(),
            "failed local intent writes must not fall through to dispatch"
        );
    }

    #[tokio::test]
    async fn discord_watch_serializes_concurrent_threshold_updates() {
        let (tx, mut rx) = mpsc::channel(5);
        let dir = tempdir().expect("tempdir");
        let intents = dir.path().join("discord-watch-intents.jsonl");
        let mut config = AppConfig::default();
        config.discord_watch.enabled = true;
        config.discord_watch.gaebal_gajae_user_id = "fixture-gaebal".into();
        config.discord_watch.watched_channels = vec![crate::config::DiscordWatchChannel {
            id: "fixture-general".into(),
            name: "general".into(),
        }];
        config.discord_watch.global_cooldown_ms = 0;
        config.discord_watch.channel_cooldown_ms = 0;
        config.discord_watch.state_file = Some(dir.path().join("discord-watch-state.json"));
        config.discord_watch.intent_file = Some(intents.clone());
        let gaebal = config.discord_watch.gaebal_gajae_user_id.clone();
        let state = AppState {
            config: Arc::new(config),
            port: 25294,
            tx,
            tmux_registry: Arc::new(RwLock::new(HashMap::new())),
            pending_update: update::new_shared_pending_update(),
            native_observability: new_shared_native_hook_observability(),
            source_health: new_shared_source_health(),
            cron_state_path: dir.path().join("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };

        let event = |id: &str| IncomingEvent {
            kind: "discord.message-create".into(),
            channel: Some("fixture-general".into()),
            mention: None,
            format: None,
            template: None,
            payload: json!({
                "message_id": id,
                "channel_id": "fixture-general",
                "channel_name": "general",
                "author_id": "user",
                "content": format!("<@{gaebal}>"),
                "mentions": [gaebal.as_str()],
                "direct_message": false,
                "author_is_owner": false,
                "timestamp_ms": 1000
            }),
        };

        let (r1, r2, r3, r4, r5) = tokio::join!(
            accept_event(&state, event("m1")),
            accept_event(&state, event("m2")),
            accept_event(&state, event("m3")),
            accept_event(&state, event("m4")),
            accept_event(&state, event("m5")),
        );
        for response in [r1, r2, r3, r4, r5] {
            assert_eq!(response.status(), StatusCode::ACCEPTED);
        }

        assert!(
            timeout(Duration::from_millis(25), rx.recv()).await.is_err(),
            "discord watch ingress must remain local-only under concurrency"
        );
        let jsonl = fs::read_to_string(intents).expect("intent jsonl");
        assert_eq!(jsonl.lines().count(), 1);
        assert!(jsonl.contains("t1-pending-mentions"));
    }

    #[tokio::test]
    async fn post_native_hook_observability_counts_accepted_event() {
        let repo = git_repo();
        let payload = native_payload(repo.path(), "SessionStart");
        let (tx, mut rx) = mpsc::channel(1);
        let observability = new_shared_native_hook_observability();
        let state = AppState {
            config: Arc::new(AppConfig::default()),
            port: 25294,
            tx,
            tmux_registry: Arc::new(RwLock::new(HashMap::new())),
            pending_update: update::new_shared_pending_update(),
            native_observability: observability.clone(),
            source_health: new_shared_source_health(),
            cron_state_path: PathBuf::from("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };

        let response = post_native_hook(State(state), Json(payload))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let queued = rx.recv().await.expect("queued event");
        assert_eq!(queued.kind, "session.started");

        let snapshot = snapshot_shared(&observability);
        assert_eq!(snapshot["totals"]["received"], json!(1));
        assert_eq!(snapshot["totals"]["normalized"], json!(1));
        assert_eq!(snapshot["recent_groups"][0]["provider"], json!("codex"));
    }

    #[tokio::test]
    async fn post_native_hook_observability_counts_rejected_event() {
        let (tx, _rx) = mpsc::channel(1);
        let observability = new_shared_native_hook_observability();
        let state = AppState {
            config: Arc::new(AppConfig::default()),
            port: 25294,
            tx,
            tmux_registry: Arc::new(RwLock::new(HashMap::new())),
            pending_update: update::new_shared_pending_update(),
            native_observability: observability.clone(),
            source_health: new_shared_source_health(),
            cron_state_path: PathBuf::from("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };
        let payload = json!({"provider": "codex", "event_name": "Bogus"});

        let response = post_native_hook(State(state), Json(payload))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let snapshot = snapshot_shared(&observability);
        assert_eq!(snapshot["totals"]["received"], json!(1));
        assert_eq!(snapshot["totals"]["dropped"], json!(1));
        assert_eq!(snapshot["reasons"]["normalization_failed"], json!(1));
    }

    #[tokio::test]
    async fn post_native_hook_observability_counts_non_git_drop() {
        let (tx, mut rx) = mpsc::channel(1);
        let observability = new_shared_native_hook_observability();
        let state = AppState {
            config: Arc::new(AppConfig::default()),
            port: 25294,
            tx,
            tmux_registry: Arc::new(RwLock::new(HashMap::new())),
            pending_update: update::new_shared_pending_update(),
            native_observability: observability.clone(),
            source_health: new_shared_source_health(),
            cron_state_path: PathBuf::from("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };
        let dir = tempdir().expect("tempdir");
        let payload = json!({
            "provider": "codex",
            "event_name": "SessionStart",
            "directory": dir.path(),
            "event_payload": {}
        });

        let response = post_native_hook(State(state), Json(payload))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert!(rx.try_recv().is_err());

        let snapshot = snapshot_shared(&observability);
        assert_eq!(snapshot["totals"]["received"], json!(1));
        assert_eq!(snapshot["totals"]["normalized"], json!(1));
        assert_eq!(snapshot["totals"]["dropped"], json!(1));
        assert_eq!(snapshot["reasons"]["non_git"], json!(1));
    }

    #[tokio::test]
    async fn post_native_hook_observability_counts_stale_defer() {
        let repo = git_repo();
        let mut payload = native_payload(repo.path(), "PostToolUse");
        payload
            .as_object_mut()
            .unwrap()
            .insert("timestamp".into(), Value::String(stale_rfc3339()));
        let (tx, mut rx) = mpsc::channel(1);
        let observability = new_shared_native_hook_observability();
        let state = AppState {
            config: Arc::new(AppConfig::default()),
            port: 25294,
            tx,
            tmux_registry: Arc::new(RwLock::new(HashMap::new())),
            pending_update: update::new_shared_pending_update(),
            native_observability: observability.clone(),
            source_health: new_shared_source_health(),
            cron_state_path: PathBuf::from("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };

        let response = post_native_hook(State(state), Json(payload))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert!(rx.try_recv().is_err());

        let snapshot = snapshot_shared(&observability);
        assert_eq!(snapshot["totals"]["received"], json!(1));
        assert_eq!(snapshot["totals"]["normalized"], json!(1));
        assert_eq!(snapshot["totals"]["deferred"], json!(1));
        assert_eq!(snapshot["reasons"]["stale_replay"], json!(1));
    }

    #[tokio::test]
    async fn post_native_hook_accepts_codex_payload_and_queues_normalized_event() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("op_pi");
        std::fs::create_dir_all(&repo).expect("create repo");
        let git = std::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .output()
            .expect("git init");
        assert!(
            git.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&git.stderr)
        );
        let (tx, mut rx) = mpsc::channel(1);
        let state = AppState {
            config: Arc::new(AppConfig::default()),
            port: 25294,
            tx,
            tmux_registry: Arc::new(RwLock::new(HashMap::new())),
            pending_update: update::new_shared_pending_update(),
            native_observability: new_shared_native_hook_observability(),
            source_health: new_shared_source_health(),
            cron_state_path: PathBuf::from("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };
        let payload = json!({
            "provider": "codex",
            "event_name": "SessionStart",
            "directory": repo,
            "cwd": repo,
            "event_payload": {
                "session_id": "sess-65",
                "cwd": repo
            }
        });

        let response = post_native_hook(State(state), Json(payload))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let response_json: Value = serde_json::from_slice(&body).unwrap();
        let event_id = response_json["event_id"].as_str().unwrap();
        assert!(!event_id.is_empty());
        assert_eq!(response_json["type"], Value::from("session.started"));

        let queued = rx.recv().await.unwrap();
        assert_eq!(queued.kind, "session.started");
        assert_eq!(queued.payload["tool"], Value::from("codex"));
        assert_eq!(queued.payload["session_id"], Value::from("sess-65"));
        assert_eq!(queued.payload["event_id"], Value::from(event_id));
    }

    #[tokio::test]
    async fn post_native_hook_queues_ask_tool_as_session_blocked() {
        let repo = git_repo();
        let mut payload = native_payload(repo.path(), "PreToolUse");
        payload["provider"] = json!("claude-code");
        payload["event_payload"]["tool_name"] = json!("askuserquestion");
        payload["event_payload"]["tool_input"] = json!({
            "question": "Need operator approval?\nDo not dump the full transcript."
        });

        let (response_json, mut rx) = post_native_payload(payload).await;
        assert_eq!(response_json["ok"], json!(true));
        assert_eq!(response_json["type"], json!("session.blocked"));

        let queued = rx.recv().await.expect("queued event");
        assert_eq!(queued.kind, "session.blocked");
        assert_eq!(queued.payload["tool"], json!("claude-code"));
        assert_eq!(queued.payload["agent_name"], json!("claude-code"));
        assert_eq!(queued.payload["route_key"], json!("question.requested"));
        assert_eq!(
            queued.payload["summary"],
            json!("Need operator approval? Do not dump the full transcript.")
        );
        assert_eq!(queued.payload["event_payload"]["redacted"], json!(true));
        assert!(queued.payload["event_payload"].get("tool_input").is_none());
    }

    #[tokio::test]
    async fn post_native_hook_rejects_unsupported_event() {
        let (tx, _rx) = mpsc::channel(1);
        let state = AppState {
            config: Arc::new(AppConfig::default()),
            port: 25294,
            tx,
            tmux_registry: Arc::new(RwLock::new(HashMap::new())),
            pending_update: update::new_shared_pending_update(),
            native_observability: new_shared_native_hook_observability(),
            source_health: new_shared_source_health(),
            cron_state_path: PathBuf::from("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };
        let payload = json!({
            "provider": "claude-code",
            "event_name": "Notification",
            "directory": "/repo/op_pi",
            "event_payload": {}
        });

        let response = post_native_hook(State(state), Json(payload))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let response_json: Value = serde_json::from_slice(&body).unwrap();
        assert!(
            response_json["error"]
                .as_str()
                .is_some_and(|error| error.contains("unsupported native hook event"))
        );
    }

    #[tokio::test]
    async fn post_native_hook_defers_stale_payloads_from_all_trusted_timestamp_paths() {
        let repo = git_repo();
        let cases = [
            vec!["event_timestamp"],
            vec!["timestamp"],
            vec!["observed_at"],
            vec!["created_at"],
            vec!["event_payload", "event_timestamp"],
            vec!["event_payload", "timestamp"],
        ];

        for path in cases {
            let mut payload = native_payload(repo.path(), "PostToolUse");
            insert_timestamp_at_path(&mut payload, &path, stale_rfc3339());

            let (response_json, mut rx) = post_native_payload(payload).await;
            assert_eq!(response_json["ok"], json!(true));
            assert_eq!(response_json["type"], json!("tool.post"));
            assert_eq!(response_json["deferred"], json!(true));
            assert_eq!(response_json["quarantined"], json!(true));
            assert_eq!(response_json["reason"], json!(STALE_NATIVE_REPLAY_REASON));
            assert!(
                rx.try_recv().is_err(),
                "stale payload at {path:?} should not enqueue"
            );
        }
    }

    #[tokio::test]
    async fn post_native_hook_defers_all_replay_sensitive_native_kinds() {
        let repo = git_repo();
        let cases = [
            ("PreToolUse", "tool.pre"),
            ("PostToolUse", "tool.post"),
            ("UserPromptSubmit", "session.prompt-submitted"),
            ("Stop", "session.stopped"),
        ];

        for (event_name, expected_kind) in cases {
            let mut payload = native_payload(repo.path(), event_name);
            payload
                .as_object_mut()
                .unwrap()
                .insert("timestamp".into(), Value::String(stale_rfc3339()));

            let (response_json, mut rx) = post_native_payload(payload).await;
            assert_eq!(response_json["type"], json!(expected_kind));
            assert_eq!(response_json["deferred"], json!(true));
            assert!(
                rx.try_recv().is_err(),
                "{expected_kind} stale replay should not enqueue"
            );
        }
    }

    #[tokio::test]
    async fn post_native_hook_stale_session_started_still_enqueues() {
        let repo = git_repo();
        let mut payload = native_payload(repo.path(), "SessionStart");
        payload
            .as_object_mut()
            .unwrap()
            .insert("timestamp".into(), Value::String(stale_rfc3339()));

        let (response_json, mut rx) = post_native_payload(payload).await;
        assert_eq!(response_json["type"], json!("session.started"));
        assert!(response_json.get("deferred").is_none());
        let queued = rx.recv().await.expect("queued event");
        assert_eq!(queued.kind, "session.started");
    }

    #[tokio::test]
    async fn post_native_hook_preserves_fresh_timestamped_tool_post() {
        let repo = git_repo();
        let mut payload = native_payload(repo.path(), "PostToolUse");
        payload
            .as_object_mut()
            .unwrap()
            .insert("timestamp".into(), Value::String(fresh_rfc3339()));

        let (response_json, mut rx) = post_native_payload(payload).await;
        assert_eq!(response_json["type"], json!("tool.post"));
        assert!(response_json["event_id"].as_str().is_some());
        let queued = rx.recv().await.expect("queued event");
        assert_eq!(queued.kind, "tool.post");
    }

    #[tokio::test]
    async fn post_native_hook_preserves_timestampless_tool_post() {
        let repo = git_repo();
        let payload = native_payload(repo.path(), "PostToolUse");

        let (response_json, mut rx) = post_native_payload(payload).await;
        assert_eq!(response_json["type"], json!("tool.post"));
        assert!(response_json["event_id"].as_str().is_some());
        let queued = rx.recv().await.expect("queued event");
        assert_eq!(queued.kind, "tool.post");
    }

    #[tokio::test]
    async fn post_native_hook_invalid_timestamp_enqueues() {
        let repo = git_repo();
        let mut payload = native_payload(repo.path(), "PostToolUse");
        payload
            .as_object_mut()
            .unwrap()
            .insert("timestamp".into(), Value::String("not-a-time".into()));

        let (response_json, mut rx) = post_native_payload(payload).await;
        assert_eq!(response_json["type"], json!("tool.post"));
        assert!(response_json.get("deferred").is_none());
        let queued = rx.recv().await.expect("queued event");
        assert_eq!(queued.kind, "tool.post");
    }

    #[tokio::test]
    async fn post_native_hook_does_not_treat_stop_context_last_prompt_at_as_event_timestamp() {
        let repo = git_repo();
        let mut payload = native_payload(repo.path(), "Stop");
        payload.as_object_mut().unwrap().insert(
            "stop_context".into(),
            json!({ "last_prompt_at": stale_rfc3339() }),
        );

        let (response_json, mut rx) = post_native_payload(payload).await;
        assert_eq!(response_json["type"], json!("session.stopped"));
        assert!(response_json.get("deferred").is_none());
        let queued = rx.recv().await.expect("queued event");
        assert_eq!(queued.kind, "session.stopped");
    }

    #[tokio::test]
    async fn post_native_hook_accepts_but_drops_non_git_payloads_before_enqueue() {
        let (tx, mut rx) = mpsc::channel(1);
        let state = AppState {
            config: Arc::new(AppConfig::default()),
            port: 25294,
            tx,
            tmux_registry: Arc::new(RwLock::new(HashMap::new())),
            pending_update: update::new_shared_pending_update(),
            native_observability: new_shared_native_hook_observability(),
            source_health: new_shared_source_health(),
            cron_state_path: PathBuf::from("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };
        let dir = tempdir().expect("tempdir");
        let payload = json!({
            "provider": "codex",
            "event_name": "SessionStart",
            "directory": dir.path(),
            "event_payload": {},
            "normalization_outcome": NATIVE_NON_GIT_OUTCOME
        });

        let response = post_native_hook(State(state), Json(payload))
            .await
            .into_response();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let response_json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(response_json["ok"], json!(true));
        assert_eq!(response_json["dropped"], json!(true));
        assert_eq!(response_json["reason"], json!(NATIVE_NON_GIT_OUTCOME));
        assert!(rx.try_recv().is_err(), "non-git payload should not enqueue");
    }

    #[tokio::test]
    async fn list_tmux_returns_registered_sessions_with_metadata() {
        let (tx, _rx) = mpsc::channel(1);
        let registry: SharedTmuxRegistry = Arc::new(RwLock::new(HashMap::new()));
        registry.write().await.insert(
            "issue-105".into(),
            RegisteredTmuxSession {
                session: "issue-105".into(),
                channel: Some("alerts".into()),
                mention: Some("<@123>".into()),
                routing: RoutingMetadata::default(),
                keywords: vec!["error".into()],
                keyword_window_secs: 30,
                stale_minutes: 15,
                format: None,
                registered_at: "2026-04-02T00:00:00Z".into(),
                registration_source: RegistrationSource::CliWatch,
                parent_process: Some(ParentProcessInfo {
                    pid: 4242,
                    name: Some("codex".into()),
                }),
                active_wrapper_monitor: true,
            },
        );
        let state = AppState {
            config: Arc::new(AppConfig::default()),
            port: 25294,
            tx,
            tmux_registry: registry,
            pending_update: update::new_shared_pending_update(),
            native_observability: new_shared_native_hook_observability(),
            source_health: new_shared_source_health(),
            cron_state_path: PathBuf::from("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };

        let response = list_tmux(State(state)).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let response_json: Value = serde_json::from_slice(&body).unwrap();
        let registrations = response_json.as_array().unwrap();
        assert_eq!(registrations.len(), 1);
        assert_eq!(registrations[0]["session"], Value::from("issue-105"));
        assert_eq!(
            registrations[0]["registration_source"],
            Value::from("cli-watch")
        );
        assert_eq!(
            registrations[0]["registered_at"],
            Value::from("2026-04-02T00:00:00Z")
        );
        assert_eq!(registrations[0]["parent_process"]["pid"], Value::from(4242));
        assert_eq!(
            registrations[0]["parent_process"]["name"],
            Value::from("codex")
        );
    }

    #[tokio::test]
    async fn update_status_returns_no_pending_when_empty() {
        let (tx, _rx) = mpsc::channel(1);
        let state = AppState {
            config: Arc::new(AppConfig::default()),
            port: 25294,
            tx,
            tmux_registry: Arc::new(RwLock::new(HashMap::new())),
            pending_update: update::new_shared_pending_update(),
            native_observability: new_shared_native_hook_observability(),
            source_health: new_shared_source_health(),
            cron_state_path: PathBuf::from("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };

        let response = update_status(State(state)).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["pending"], Value::Bool(false));
        assert_eq!(json["current_version"], Value::String(VERSION.to_string()));
    }

    #[tokio::test]
    async fn update_status_returns_pending_when_set() {
        let (tx, _rx) = mpsc::channel(1);
        let pending = update::new_shared_pending_update();
        *pending.write().await = Some(update::PendingUpdate {
            current_version: "0.5.4".into(),
            latest_version: "0.6.0".into(),
            release_url: "https://github.com/IYENTeam/op_pi/releases/tag/v0.6.0".into(),
            detected_at: "2026-04-07T00:00:00Z".into(),
        });

        let state = AppState {
            config: Arc::new(AppConfig::default()),
            port: 25294,
            tx,
            tmux_registry: Arc::new(RwLock::new(HashMap::new())),
            pending_update: pending,
            native_observability: new_shared_native_hook_observability(),
            source_health: new_shared_source_health(),
            cron_state_path: PathBuf::from("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };

        let response = update_status(State(state)).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["pending"], Value::Bool(true));
        assert_eq!(json["latest_version"], Value::from("0.6.0"));
        assert_eq!(json["current_version"], Value::from("0.5.4"));
    }

    #[tokio::test]
    async fn approve_returns_error_when_no_pending_update() {
        let (tx, _rx) = mpsc::channel(1);
        let state = AppState {
            config: Arc::new(AppConfig::default()),
            port: 25294,
            tx,
            tmux_registry: Arc::new(RwLock::new(HashMap::new())),
            pending_update: update::new_shared_pending_update(),
            native_observability: new_shared_native_hook_observability(),
            source_health: new_shared_source_health(),
            cron_state_path: PathBuf::from("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };

        let response = approve_update(State(state)).await.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["ok"], Value::Bool(false));
        assert!(
            json["error"]
                .as_str()
                .unwrap()
                .contains("no pending update")
        );
    }

    #[tokio::test]
    async fn dismiss_clears_pending_update() {
        let (tx, _rx) = mpsc::channel(1);
        let pending = update::new_shared_pending_update();
        *pending.write().await = Some(update::PendingUpdate {
            current_version: "0.5.4".into(),
            latest_version: "0.6.0".into(),
            release_url: "https://example.com".into(),
            detected_at: "2026-04-07T00:00:00Z".into(),
        });

        let state = AppState {
            config: Arc::new(AppConfig::default()),
            port: 25294,
            tx,
            tmux_registry: Arc::new(RwLock::new(HashMap::new())),
            pending_update: pending.clone(),
            native_observability: new_shared_native_hook_observability(),
            source_health: new_shared_source_health(),
            cron_state_path: PathBuf::from("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };

        let response = dismiss_update(State(state)).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["ok"], Value::Bool(true));
        assert_eq!(json["dismissed_version"], Value::from("0.6.0"));
        assert!(pending.read().await.is_none());
    }

    #[tokio::test]
    async fn dismiss_returns_error_when_no_pending_update() {
        let (tx, _rx) = mpsc::channel(1);
        let state = AppState {
            config: Arc::new(AppConfig::default()),
            port: 25294,
            tx,
            tmux_registry: Arc::new(RwLock::new(HashMap::new())),
            pending_update: update::new_shared_pending_update(),
            native_observability: new_shared_native_hook_observability(),
            source_health: new_shared_source_health(),
            cron_state_path: PathBuf::from("cron-state.json"),
            discord_watch_lock: Arc::new(Mutex::new(())),
            sns_cert_cache: Arc::new(crate::intake::SnsCertCache::new()),
            calendar_webhook_rate_limit: Arc::new(Mutex::new(RateLimiter::new(
                CALENDAR_WEBHOOK_RATE_LIMIT_CAPACITY,
                CALENDAR_WEBHOOK_RATE_LIMIT_REFILL_PER_SEC,
            ))),
        };

        let response = dismiss_update(State(state)).await.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["ok"], Value::Bool(false));
    }

    #[tokio::test]
    async fn aws_sns_rejects_non_allowlisted_topic() {
        let mut config = AppConfig::default();
        config.aws.topic_allowlist = vec!["arn:aws:sns:us-east-1:123456789012:op_pi-alarms".into()];
        let (state, _rx) = app_state_with_config(config);

        let response = post_aws_sns(
            State(state),
            Json(json!({
                "Type": "Notification",
                "MessageId": "evil",
                "TopicArn": "arn:aws:sns:us-east-1:999999999999:evil",
                "Message": "hi"
            })),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn aws_sns_enqueues_legacy_signed_cloudwatch_alarm_event() {
        const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sns");
        const CERT_URL: &str =
            "https://sns.us-east-1.amazonaws.com/SimpleNotificationService-test.pem";
        let (state, mut rx) = app_state_with_config(AppConfig::default());
        let cert_pem = std::fs::read_to_string(format!("{FIXTURE_DIR}/cert.pem")).unwrap();
        let (key, not_after) = crate::intake::parse_signing_cert(&cert_pem).unwrap();
        state.sns_cert_cache.insert(CERT_URL, key, not_after);

        let response = post_aws_sns(
            State(state),
            Json(json!({
                "Type": "Notification",
                "MessageId": "22b80b92",
                "TopicArn": "arn:aws:sns:us-east-1:123456789012:op_pi-alarms",
                "Subject": "ALARM: ServerCpuTooHigh",
                "Message": "{\"AlarmName\":\"ServerCpuTooHigh\",\"NewStateValue\":\"ALARM\"}",
                "Timestamp": "2026-07-22T12:00:01.000Z",
                "SignatureVersion": "2",
                "Signature": std::fs::read_to_string(format!("{FIXTURE_DIR}/sig.b64")).unwrap().trim(),
                "SigningCertURL": CERT_URL
            })),
        )
        .await;

        assert!(response.status().is_success());
        let event = rx.recv().await.expect("event should be enqueued");
        assert_eq!(event.kind, "aws.cloudwatch-alarm");
    }

    #[tokio::test]
    async fn aws_sns_rejects_unsigned_forged_alarm() {
        let (state, _rx) = app_state_with_config(AppConfig::default());

        let response = post_aws_sns(
            State(state),
            Json(json!({
                "Type": "Notification",
                "MessageId": "forged",
                "TopicArn": "arn:aws:sns:us-east-1:123456789012:op_pi-alarms",
                "Message": "{\"AlarmName\":\"Forged\",\"NewStateValue\":\"ALARM\"}",
                "Timestamp": "2026-07-22T12:00:01.000Z"
            })),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn aws_eventbridge_unconfigured_returns_503() {
        let (state, _rx) = app_state_with_config(AppConfig::default());

        let response = post_aws_eventbridge(
            State(state),
            HeaderMap::new(),
            Json(json!({"source": "aws.ec2", "detail-type": "EC2 Instance State-change Notification"})),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn cloudflare_endpoints_unconfigured_return_503() {
        let (state, _rx) = app_state_with_config(AppConfig::default());
        let response = post_cloudflare_notification(
            State(state),
            HeaderMap::new(),
            Json(json!({"alert_type": "x"})),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let (state, _rx) = app_state_with_config(AppConfig::default());
        let response = post_cloudflare_logpush(
            State(state),
            HeaderMap::new(),
            axum::extract::Query(std::collections::BTreeMap::new()),
            axum::body::Bytes::from("{}\n"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn general_event_endpoint_cannot_dispatch_google_calendar_callbacks() {
        let (state, mut rx) = app_state_with_config(AppConfig::default());
        let response = post_event(
            State(state),
            Json(IncomingEvent {
                kind: "google.calendar.changed".into(),
                channel: None,
                mention: None,
                format: None,
                template: None,
                payload: json!({"message_number": 42}),
            }),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(rx.try_recv().is_err(), "forged callback must not dispatch");
    }

    #[tokio::test]
    async fn google_calendar_unconfigured_returns_503() {
        let (state, _rx) = app_state_with_config(AppConfig::default());

        let response = post_google_calendar(State(state), HeaderMap::new()).await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn google_calendar_webhook_only_mode_authenticates_and_dispatches_normalized_callbacks() {
        let config = AppConfig {
            google_calendar: crate::config::GoogleCalendarConfig {
                channel_token: Some("calendar-secret".into()),
                ..crate::config::GoogleCalendarConfig::default()
            },
            ..AppConfig::default()
        };

        let mut missing_token_headers = google_calendar_headers("unused", "exists");
        missing_token_headers.remove("x-goog-channel-token");
        let (state, _rx) = app_state_with_config(config.clone());
        let response = post_google_calendar(State(state), missing_token_headers).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let (state, _rx) = app_state_with_config(config.clone());
        let response =
            post_google_calendar(State(state), google_calendar_headers("wrong", "exists")).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let mut missing_required_header = google_calendar_headers("calendar-secret", "exists");
        missing_required_header.remove("x-goog-resource-uri");
        let (state, _rx) = app_state_with_config(config.clone());
        let response = post_google_calendar(State(state), missing_required_header).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let (state, mut rx) = app_state_with_config(config);
        let response = post_google_calendar(
            State(state),
            google_calendar_headers("calendar-secret", "exists"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let event = rx
            .recv()
            .await
            .expect("authenticated webhook-only callback should be dispatched");
        assert_eq!(event.kind, "google.calendar.changed");
        assert_eq!(event.payload["channel_id"], "channel-1");
        assert_eq!(event.payload["message_number"], 42);
    }

    #[tokio::test]
    async fn google_calendar_durable_sync_dispatches_only_source_accepted_callbacks() {
        let mut config = AppConfig::default();
        config.google_calendar.channel_token = Some("calendar-secret".into());
        config.google_calendar.credentials_file = Some("oauth.json".into());
        config.google_calendar.state_file = Some("calendar-state.json".into());
        let (state, mut rx) = app_state_with_config(config);
        let (notifications, mut notification_rx) = mpsc::channel::<CalendarNotification>(1);
        tokio::spawn(async move {
            let notification = notification_rx.recv().await.expect("notification received");
            notification
                .persisted
                .send(true)
                .expect("send accepted validation");
        });

        let response = post_google_calendar_with_source(
            State(state),
            Extension(notifications),
            google_calendar_headers("calendar-secret", "exists"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let event = rx
            .recv()
            .await
            .expect("source-accepted callback should be dispatched");
        assert_eq!(event.kind, "google.calendar.changed");
    }

    #[tokio::test]
    async fn google_calendar_untracked_or_replayed_callback_is_acknowledged_without_dispatch() {
        let mut config = AppConfig::default();
        config.google_calendar.channel_token = Some("calendar-secret".into());
        config.google_calendar.credentials_file = Some("oauth.json".into());
        config.google_calendar.state_file = Some("calendar-state.json".into());
        let (state, mut rx) = app_state_with_config(config);
        let (notifications, mut notification_rx) = mpsc::channel::<CalendarNotification>(1);
        tokio::spawn(async move {
            let notification = notification_rx.recv().await.expect("notification received");
            notification
                .persisted
                .send(false)
                .expect("send rejected validation");
        });

        let response = post_google_calendar_with_source(
            State(state),
            Extension(notifications),
            google_calendar_headers("calendar-secret", "exists"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        let acknowledgement: Value = serde_json::from_slice(&body).expect("acknowledgement JSON");
        assert_eq!(acknowledgement["trusted"], false);
        assert_eq!(acknowledgement["reason"], "untracked_or_replayed");
        assert!(
            rx.try_recv().is_err(),
            "untrusted callback must not dispatch"
        );
    }

    #[tokio::test]
    async fn google_calendar_closed_notification_receiver_returns_503_without_journaling() {
        let directory = tempdir().expect("temporary calendar directory");
        let mut config = AppConfig::default();
        config.google_calendar.channel_token = Some("calendar-secret".into());
        config.google_calendar.credentials_file = Some(directory.path().join("oauth.json"));
        config.google_calendar.state_file = Some(directory.path().join("calendar-state.json"));
        let (state, mut rx) = app_state_with_config(config);
        let (notifications, notification_rx) = mpsc::channel::<CalendarNotification>(1);
        drop(notification_rx);

        let response = post_google_calendar_with_source(
            State(state),
            Extension(notifications),
            google_calendar_headers("calendar-secret", "exists"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(rx.try_recv().is_err(), "closed receiver must not dispatch");
        assert!(
            fs::read_dir(directory.path())
                .expect("read temporary calendar directory")
                .next()
                .is_none(),
            "closed receiver must not journal"
        );
    }

    #[tokio::test]
    async fn google_calendar_timed_out_notification_delivery_is_durably_deferred() {
        let directory = tempdir().expect("temporary calendar directory");
        let mut config = AppConfig::default();
        config.google_calendar.channel_token = Some("calendar-secret".into());
        config.google_calendar.credentials_file = Some(directory.path().join("oauth.json"));
        config.google_calendar.state_file = Some(directory.path().join("calendar-state.json"));
        let (state, mut rx) = app_state_with_config(config);
        let (notifications, _notification_rx) = mpsc::channel::<CalendarNotification>(1);
        let (persisted, _persisted_ack) = oneshot::channel();
        notifications
            .try_send(CalendarNotification {
                channel_id: "queued-channel".into(),
                resource_id: "queued-resource".into(),
                resource_uri: "https://www.googleapis.com/calendar/v3/calendars/team/events".into(),
                message_number: 1,
                resource_state: "exists".into(),
                persisted,
            })
            .expect("fill notification queue");

        let response = post_google_calendar_with_source(
            State(state),
            Extension(notifications),
            google_calendar_headers("calendar-secret", "exists"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        let acknowledgement: Value = serde_json::from_slice(&body).expect("acknowledgement JSON");
        assert_eq!(acknowledgement["reason"], "deferred");
        assert!(
            rx.try_recv().is_err(),
            "deferred callback must not dispatch"
        );
        assert!(
            fs::read_dir(directory.path())
                .expect("read temporary calendar directory")
                .next()
                .is_some(),
            "timed out delivery must journal the callback"
        );
    }

    #[tokio::test]
    async fn google_calendar_rate_limit_retries_authenticated_callbacks_without_dispatch() {
        let mut config = AppConfig::default();
        config.google_calendar.channel_token = Some("calendar-secret".into());
        config.google_calendar.credentials_file = Some("oauth.json".into());
        config.google_calendar.state_file = Some("calendar-state.json".into());
        let (mut state, mut rx) = app_state_with_config(config);
        state.calendar_webhook_rate_limit = Arc::new(Mutex::new(RateLimiter::new(1, 0.0)));
        let (notifications, mut notification_rx) = mpsc::channel::<CalendarNotification>(1);
        tokio::spawn(async move {
            let notification = notification_rx.recv().await.expect("notification received");
            notification
                .persisted
                .send(true)
                .expect("send accepted validation");
        });

        let first = post_google_calendar_with_source(
            State(state.clone()),
            Extension(notifications.clone()),
            google_calendar_headers("calendar-secret", "exists"),
        )
        .await;
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        let _ = rx
            .recv()
            .await
            .expect("accepted callback should be dispatched");

        let second = post_google_calendar_with_source(
            State(state),
            Extension(notifications),
            google_calendar_headers("calendar-secret", "exists"),
        )
        .await;
        assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = to_bytes(second.into_body(), usize::MAX)
            .await
            .expect("response body");
        assert_eq!(
            body.as_ref(),
            b"Google Calendar notification rate limit exceeded"
        );
        assert!(
            rx.try_recv().is_err(),
            "rate-limited callback must not dispatch"
        );
    }

    fn google_calendar_headers(token: &str, resource_state: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-goog-channel-id", "channel-1".parse().unwrap());
        headers.insert("x-goog-channel-token", token.parse().unwrap());
        headers.insert("x-goog-resource-id", "resource-1".parse().unwrap());
        headers.insert(
            "x-goog-resource-uri",
            "https://www.googleapis.com/calendar/v3/calendars/team/events"
                .parse()
                .unwrap(),
        );
        headers.insert("x-goog-resource-state", resource_state.parse().unwrap());
        headers.insert("x-goog-message-number", "42".parse().unwrap());
        headers
    }

    #[tokio::test]
    async fn aws_eventbridge_requires_secret_when_configured() {
        let mut config = AppConfig::default();
        config.aws.webhook_secret = Some("eb-secret".into());
        let payload = json!({
            "source": "aws.guardduty",
            "detail-type": "GuardDuty Finding",
            "id": "abc",
            "detail": {"severity": 8.0}
        });

        let (state, _rx) = app_state_with_config(config.clone());
        let response =
            post_aws_eventbridge(State(state), HeaderMap::new(), Json(payload.clone())).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let (state, mut rx) = app_state_with_config(config);
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", "eb-secret".parse().unwrap());
        let response = post_aws_eventbridge(State(state), headers, Json(payload)).await;
        assert!(response.status().is_success());
        let event = rx.recv().await.expect("event should be enqueued");
        assert_eq!(event.kind, "aws.eventbridge.guardduty-finding");
    }

    #[tokio::test]
    async fn cloudflare_notification_rejects_wrong_secret_and_accepts_correct() {
        let mut config = AppConfig::default();
        config.cloudflare.webhook_secret = Some("cf-secret".into());
        let payload = json!({
            "alert_type": "health_check_status_notification",
            "text": "origin-api unhealthy",
            "data": {"new_health_status": "unhealthy"}
        });

        let (state, _rx) = app_state_with_config(config.clone());
        let mut headers = HeaderMap::new();
        headers.insert("cf-webhook-auth", "wrong".parse().unwrap());
        let response =
            post_cloudflare_notification(State(state), headers, Json(payload.clone())).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let (state, mut rx) = app_state_with_config(config);
        let mut headers = HeaderMap::new();
        headers.insert("cf-webhook-auth", "cf-secret".parse().unwrap());
        let response = post_cloudflare_notification(State(state), headers, Json(payload)).await;
        assert!(response.status().is_success());
        let event = rx.recv().await.expect("event should be enqueued");
        assert_eq!(event.kind, "cloudflare.health_check_status_notification");
    }

    fn configured_linear_state() -> (AppState, mpsc::Receiver<IncomingEvent>) {
        let mut config = AppConfig::default();
        config.linear.webhook_secret = Some("linear-test-secret".into());
        app_state_with_config(config)
    }

    fn current_linear_body() -> Vec<u8> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        serde_json::to_vec(&json!({
            "type": "IssueLabel",
            "action": "update",
            "webhookTimestamp": now_ms,
            "data": {"id": "lbl_1"},
        }))
        .unwrap()
    }

    fn linear_signature(body: &[u8]) -> String {
        use hmac::{Hmac, Mac};
        type HmacSha256 = Hmac<sha2::Sha256>;
        let mut mac = HmacSha256::new_from_slice(b"linear-test-secret").unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    fn linear_request(body: Vec<u8>, signature: Option<String>) -> Request<Body> {
        let mut builder = Request::post("/linear");
        if let Some(signature) = signature {
            builder = builder.header("Linear-Signature", signature);
        }
        builder.body(Body::from(body)).unwrap()
    }

    fn linear_body_at(timestamp: u64, data_id: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "type": "IssueLabel",
            "action": "update",
            "webhookTimestamp": timestamp,
            "data": {"id": data_id},
        }))
        .unwrap()
    }

    fn app_router_with_linear(state: AppState, linear: LinearIntake) -> AxumRouter {
        let (calendar_notification_tx, _calendar_notification_rx) = mpsc::channel(1);
        app_router_with_calendar(state, calendar_notification_tx, linear, None)
    }

    #[tokio::test]
    async fn linear_router_auth_precedence_and_route_registration() {
        let (state, _rx) = configured_linear_state();
        assert_eq!(
            app_router(state)
                .oneshot(linear_request(b"not json".to_vec(), None))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );

        let (state, _rx) = configured_linear_state();
        let malformed = b"not json".to_vec();
        assert_eq!(
            app_router(state)
                .oneshot(linear_request(
                    malformed.clone(),
                    Some(linear_signature(&malformed))
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );

        let (state, _rx) = configured_linear_state();
        assert_eq!(
            app_router(state)
                .oneshot(Request::get("/linear").body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
        let (state, _rx) = configured_linear_state();
        assert_eq!(
            app_router(state)
                .oneshot(linear_request(current_linear_body(), None))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let (state, _rx) = configured_linear_state();
        assert_eq!(
            app_router(state)
                .oneshot(Request::post("/api/linear").body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        let (state, _rx) = configured_linear_state();
        assert_eq!(
            app_router(state)
                .oneshot(Request::get("/health").body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn linear_router_accepts_exact_200_and_preserves_signed_payload() {
        let (state, mut rx) = configured_linear_state();
        let body = current_linear_body();
        let response = app_router(state)
            .oneshot(linear_request(body.clone(), Some(linear_signature(&body))))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let event = rx.recv().await.expect("Linear event should be enqueued");
        assert_eq!(event.kind, "linear.issue-label-update");
        assert_eq!(
            event.payload["webhook"],
            serde_json::from_slice::<Value>(&body).unwrap()
        );
        assert_eq!(event.payload["event_id"], event.payload["correlation_id"]);
    }

    #[tokio::test]
    async fn linear_router_durably_dedupes_redelivery_via_ledger() {
        // Runtime proof of the durable plug end to end. Requires DATABASE_URL;
        // no-ops (passes) when it is unset so the suite stays green without a DB.
        let Ok(url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let ledger = evidence_ledger::EvidenceLedger::connect(&url)
            .await
            .unwrap();
        ledger.migrate().await.unwrap();

        let (state, mut rx) = configured_linear_state();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        // A body unique to this run so its digest key cannot collide with rows
        // left by other runs in the shared database.
        let body = linear_body_at(now_ms, &format!("e2e-{now_ms}"));
        let dedupe_key = hex::encode(LinearIntake::replay_identity(&body));

        let (calendar_notification_tx, _calendar_notification_rx) = mpsc::channel(1);
        let router = app_router_with_calendar(
            state,
            calendar_notification_tx,
            LinearIntake::production(),
            Some(ledger.clone()),
        );

        let first = router
            .clone()
            .oneshot(linear_request(body.clone(), Some(linear_signature(&body))))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);

        // An identical redelivery is acknowledged but deduplicated durably.
        let second = router
            .oneshot(linear_request(body.clone(), Some(linear_signature(&body))))
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);

        // The event is durably stored under the stable body-digest key...
        assert!(ledger.get(&dedupe_key).await.unwrap().is_some());
        // ...and only the first delivery reached the processing queue.
        let event = rx.recv().await.expect("first delivery enqueued");
        assert_eq!(event.kind, "linear.issue-label-update");
        assert!(
            rx.try_recv().is_err(),
            "a durable redelivery must not enqueue the event again"
        );
    }

    #[tokio::test]
    async fn linear_router_unconfigured_oversized_body_returns_503() {
        let (state, _rx) = app_state_with_config(AppConfig::default());
        let response = app_router(state)
            .oneshot(linear_request(
                vec![b'x'; crate::intake::LINEAR_MAX_BODY_BYTES + 1],
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn linear_router_preserves_valid_utf8_delivery_header() {
        let (state, mut rx) = configured_linear_state();
        let body = current_linear_body();
        let mut request = linear_request(body.clone(), Some(linear_signature(&body)));
        request.headers_mut().insert(
            "linear-delivery",
            HeaderValue::from_bytes(b"delivery-\xc3\xa9").unwrap(),
        );
        assert_eq!(
            app_router(state).oneshot(request).await.unwrap().status(),
            StatusCode::OK
        );
        assert_eq!(
            rx.recv()
                .await
                .expect("Linear event should be enqueued")
                .payload["linear_delivery"],
            "delivery-é"
        );
    }

    #[tokio::test]
    async fn linear_router_unconfigured_and_unavailable_queue_return_503() {
        let (state, _rx) = app_state_with_config(AppConfig::default());
        assert_eq!(
            app_router(state)
                .oneshot(linear_request(b"not json".to_vec(), None))
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );

        let (state, _rx) = app_state_with_config_and_capacity(
            AppConfig {
                linear: crate::config::LinearConfig {
                    webhook_secret: Some("linear-test-secret".into()),
                },
                ..AppConfig::default()
            },
            1,
        );
        state
            .tx
            .try_send(IncomingEvent::custom(None, "queue filler".into()))
            .unwrap();
        let body = current_linear_body();
        let response = app_router(state)
            .oneshot(linear_request(body.clone(), Some(linear_signature(&body))))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let (state, rx) = configured_linear_state();
        drop(rx);
        let body = current_linear_body();
        assert_eq!(
            app_router(state)
                .oneshot(linear_request(body.clone(), Some(linear_signature(&body))))
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn linear_router_suppresses_max_future_duplicate_with_injected_clock() {
        let now = Arc::new(AtomicU64::new(1_700_000_000_000));
        let clock = Arc::clone(&now);
        let linear = LinearIntake::new(32, Duration::from_secs(5), move || {
            clock.load(Ordering::Relaxed)
        });
        let (state, mut rx) = configured_linear_state();
        let timestamp = now.load(Ordering::Relaxed) + 59_000;
        let body = linear_body_at(timestamp, "future");
        let app = app_router_with_linear(state, linear);
        for signature in [
            linear_signature(&body),
            linear_signature(&body).to_uppercase(),
        ] {
            assert_eq!(
                app.clone()
                    .oneshot(linear_request(body.clone(), Some(signature)))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::OK
            );
        }
        now.fetch_add(1, Ordering::Relaxed);
        assert_eq!(
            app.oneshot(linear_request(body.clone(), Some(linear_signature(&body))))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert!(rx.recv().await.is_some());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn linear_router_full_replay_cache_returns_503_without_eviction_or_enqueue() {
        let now_ms = 1_700_000_000_000;
        let clock = Arc::new(AtomicU64::new(now_ms));
        let clock_for_intake = Arc::clone(&clock);
        let linear = LinearIntake::new(32, Duration::from_secs(5), move || {
            clock_for_intake.load(Ordering::Relaxed)
        });
        let existing = linear_body_at(now_ms, "existing");
        let existing_identity = LinearIntake::replay_identity(&existing);
        {
            let mut replay = linear.replay.lock().unwrap();
            replay.insert(existing_identity, now_ms + LINEAR_REPLAY_TTL_MS);
            for index in 1..LINEAR_REPLAY_CAPACITY {
                replay.insert([index as u8; 32], now_ms + LINEAR_REPLAY_TTL_MS);
            }
        }
        let (state, mut rx) = configured_linear_state();
        let app = app_router_with_linear(state, linear.clone());
        let unique = linear_body_at(now_ms, "unique");
        assert_eq!(
            app.clone()
                .oneshot(linear_request(
                    unique.clone(),
                    Some(linear_signature(&unique))
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            linear.replay.lock().unwrap().entries.len(),
            LINEAR_REPLAY_CAPACITY
        );
        assert!(linear.replay.lock().unwrap().contains(&existing_identity));
        assert_eq!(
            app.oneshot(linear_request(
                existing.clone(),
                Some(linear_signature(&existing)),
            ))
            .await
            .unwrap()
            .status(),
            StatusCode::OK
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn linear_router_rejects_oversized_delivery_header_without_enqueue() {
        let now_ms = 1_700_000_000_000;
        let linear = LinearIntake::new(32, Duration::from_secs(5), move || now_ms);
        let (state, mut rx) = configured_linear_state();
        let body = linear_body_at(now_ms, "delivery-limit");
        let mut oversized = linear_request(body.clone(), Some(linear_signature(&body)));
        oversized.headers_mut().insert(
            "linear-delivery",
            HeaderValue::from_bytes(&vec![b'd'; LINEAR_DELIVERY_MAX_BYTES + 1]).unwrap(),
        );
        let app = app_router_with_linear(state, linear);
        assert_eq!(
            app.clone().oneshot(oversized).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
        let mut accepted = linear_request(body.clone(), Some(linear_signature(&body)));
        accepted.headers_mut().insert(
            "linear-delivery",
            HeaderValue::from_bytes(&vec![b'd'; LINEAR_DELIVERY_MAX_BYTES]).unwrap(),
        );
        assert_eq!(
            app.oneshot(accepted).await.unwrap().status(),
            StatusCode::OK
        );
        assert!(rx.recv().await.is_some());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn linear_router_rate_limit_returns_429_without_enqueue_and_refills() {
        let now = Arc::new(AtomicU64::new(1_700_000_000_000));
        let clock = Arc::clone(&now);
        let linear = LinearIntake::new(32, Duration::from_secs(5), move || {
            clock.load(Ordering::Relaxed)
        });
        let (state, mut rx) = app_state_with_config_and_capacity(
            AppConfig {
                linear: crate::config::LinearConfig {
                    webhook_secret: Some("linear-test-secret".into()),
                },
                ..AppConfig::default()
            },
            LINEAR_RATE_BURST as usize + 1,
        );
        let app = app_router_with_linear(state, linear);
        let timestamp = now.load(Ordering::Relaxed);
        for index in 0..LINEAR_RATE_BURST {
            let body = linear_body_at(timestamp, &format!("rate-{index}"));
            assert_eq!(
                app.clone()
                    .oneshot(linear_request(body.clone(), Some(linear_signature(&body))))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::OK
            );
        }
        let limited = linear_body_at(timestamp, "limited");
        assert_eq!(
            app.clone()
                .oneshot(linear_request(
                    limited.clone(),
                    Some(linear_signature(&limited))
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        for _ in 0..LINEAR_RATE_BURST {
            assert!(rx.recv().await.is_some());
        }
        assert!(rx.try_recv().is_err());
        now.fetch_add(1_000, Ordering::Relaxed);
        let refilled = linear_body_at(timestamp, "refilled");
        assert_eq!(
            app.oneshot(linear_request(
                refilled.clone(),
                Some(linear_signature(&refilled)),
            ))
            .await
            .unwrap()
            .status(),
            StatusCode::OK
        );
        assert!(rx.recv().await.is_some());
    }

    #[tokio::test]
    async fn linear_replay_suppresses_duplicates_only_after_admission() {
        let (state, mut rx) = configured_linear_state();
        let body = current_linear_body();
        let app = app_router(state);
        assert_eq!(
            app.clone()
                .oneshot(linear_request(body.clone(), Some(linear_signature(&body))))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            app.oneshot(linear_request(
                body.clone(),
                Some(linear_signature(&body).to_uppercase()),
            ))
            .await
            .unwrap()
            .status(),
            StatusCode::OK
        );
        assert!(rx.recv().await.is_some());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn linear_queue_failure_does_not_cache_retry() {
        let (state, mut rx) = app_state_with_config_and_capacity(
            AppConfig {
                linear: crate::config::LinearConfig {
                    webhook_secret: Some("linear-test-secret".into()),
                },
                ..AppConfig::default()
            },
            1,
        );
        state
            .tx
            .try_send(IncomingEvent::custom(None, "filler".into()))
            .unwrap();
        let body = current_linear_body();
        let app = app_router(state);
        assert_eq!(
            app.clone()
                .oneshot(linear_request(body.clone(), Some(linear_signature(&body))))
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(rx.recv().await.is_some());
        assert_eq!(
            app.oneshot(linear_request(body.clone(), Some(linear_signature(&body))))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(rx.recv().await.unwrap().kind, "linear.issue-label-update");
    }

    #[test]
    fn linear_replay_identity_ignores_signature_hex_casing_and_rate_is_bounded() {
        let body = b"verified-body";
        assert_eq!(
            LinearIntake::replay_identity(body),
            LinearIntake::replay_identity(body)
        );
        let mut rate = LinearRateLimiter {
            tokens: 128.0,
            last_ms: 0,
        };
        for _ in 0..128 {
            assert!(rate.try_consume(0));
        }
        assert!(!rate.try_consume(0));
        assert!(rate.try_consume(16));
    }

    #[test]
    fn linear_replay_cache_expires_with_injected_time() {
        let mut cache = LinearReplayCache {
            entries: VecDeque::new(),
        };
        let identity = [7; 32];
        cache.insert(identity, 10 + LINEAR_REPLAY_TTL_MS);
        cache.prune(10 + LINEAR_REPLAY_TTL_MS);
        assert!(cache.contains(&identity));
        cache.prune(11 + LINEAR_REPLAY_TTL_MS);
        assert!(!cache.contains(&identity));
    }

    #[tokio::test]
    async fn linear_timeout_and_saturation_are_deterministic() {
        let (state, _rx) = configured_linear_state();
        let body = current_linear_body();
        let mut headers = HeaderMap::new();
        headers.insert("linear-signature", linear_signature(&body).parse().unwrap());
        let saturated = LinearIntake::new(1, Duration::from_secs(1), unix_timestamp_ms);
        let permit = saturated.permits.clone().acquire_owned().await.unwrap();
        assert_eq!(
            post_linear(
                State(state),
                Extension(saturated),
                Extension(None),
                headers,
                Body::from(body)
            )
            .await
            .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        drop(permit);

        let (state, _rx) = configured_linear_state();
        let body = current_linear_body();
        let mut headers = HeaderMap::new();
        headers.insert("linear-signature", linear_signature(&body).parse().unwrap());
        let timed_out = LinearIntake::new(1, Duration::from_millis(1), unix_timestamp_ms);
        let stalled_body = Body::from_stream(futures_util::stream::pending::<
            std::result::Result<axum::body::Bytes, std::convert::Infallible>,
        >());
        assert_eq!(
            post_linear(
                State(state),
                Extension(timed_out),
                Extension(None),
                headers,
                stalled_body
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );

        let (state, _rx) = configured_linear_state();
        let body = current_linear_body();
        let mut headers = HeaderMap::new();
        headers.insert("linear-signature", linear_signature(&body).parse().unwrap());
        let stream_error = Body::from_stream(futures_util::stream::once(async {
            Err::<axum::body::Bytes, std::io::Error>(std::io::Error::other("read failed"))
        }));
        assert_eq!(
            post_linear(
                State(state),
                Extension(LinearIntake::new(
                    1,
                    Duration::from_secs(1),
                    unix_timestamp_ms
                )),
                Extension(None),
                headers,
                stream_error,
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn linear_router_enforces_one_mebibyte_body_limit() {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        for (size, expected) in [
            (1_048_576usize, StatusCode::OK),
            (1_048_577usize, StatusCode::PAYLOAD_TOO_LARGE),
        ] {
            let prefix = format!(
                "{{\"type\":\"Issue\",\"action\":\"update\",\"webhookTimestamp\":{now_ms},\"data\":\""
            );
            let suffix = "\"}";
            let body = format!(
                "{prefix}{}{}",
                "x".repeat(size - prefix.len() - suffix.len()),
                suffix
            )
            .into_bytes();
            assert_eq!(body.len(), size);
            let (state, mut rx) = configured_linear_state();
            let response = app_router(state)
                .oneshot(linear_request(body.clone(), Some(linear_signature(&body))))
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            if expected == StatusCode::OK {
                assert!(rx.recv().await.is_some());
            } else {
                assert!(rx.try_recv().is_err());
            }
        }
    }

    #[tokio::test]
    async fn cloudflare_logpush_enqueues_capped_batch_event() {
        let mut config = AppConfig::default();
        config.cloudflare.logpush_secret = Some("lp-secret".into());
        let (state, mut rx) = app_state_with_config(config);

        let mut headers = HeaderMap::new();
        headers.insert("x-logpush-secret", "lp-secret".parse().unwrap());
        let params: std::collections::BTreeMap<String, String> =
            [("dataset".to_string(), "firewall_events".to_string())]
                .into_iter()
                .collect();
        let body = axum::body::Bytes::from("{\"RayID\":\"ray-1\"}\n{\"RayID\":\"ray-2\"}\n");

        let response =
            post_cloudflare_logpush(State(state), headers, axum::extract::Query(params), body)
                .await;

        assert!(response.status().is_success());
        let event = rx.recv().await.expect("event should be enqueued");
        assert_eq!(event.kind, "cloudflare.logpush.firewall_events");
        assert_eq!(event.payload["record_count"], 2);
    }
}
