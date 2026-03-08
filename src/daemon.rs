use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router as AxumRouter};
use serde_json::{Value, json};
use tokio::sync::RwLock;

use crate::Result;
use crate::config::AppConfig;
use crate::discord::DiscordClient;
use crate::events::{IncomingEvent, normalize_event};
use crate::monitor::{self, RegisteredTmuxSession, SharedTmuxRegistry};
use crate::router::Router;

#[derive(Clone)]
struct AppState {
    config: Arc<AppConfig>,
    port: u16,
    router: Arc<Router>,
    discord: Arc<DiscordClient>,
    tmux_registry: SharedTmuxRegistry,
}

pub async fn run(config: Arc<AppConfig>, port_override: Option<u16>) -> Result<()> {
    let discord = Arc::new(DiscordClient::from_config(config.clone())?);
    let router = Arc::new(Router::new(config.clone()));
    let tmux_registry: SharedTmuxRegistry = Arc::new(RwLock::new(HashMap::new()));

    tokio::spawn(monitor::run(
        config.clone(),
        router.clone(),
        discord.clone(),
        tmux_registry.clone(),
    ));

    let app = AxumRouter::new()
        .route("/health", get(health))
        .route("/api/status", get(status))
        .route("/api/event", post(post_event))
        .route("/events", post(post_event))
        .route("/api/tmux/register", post(register_tmux))
        .route("/github", post(post_github));
    let port = port_override.unwrap_or(config.daemon.port);

    let app = app.with_state(AppState {
        config: config.clone(),
        port,
        router,
        discord,
        tmux_registry,
    });
    let addr: SocketAddr = format!("{}:{}", config.daemon.bind_host, port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!(
        "clawhip daemon listening on http://{}",
        listener.local_addr()?
    );
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let registered = state.tmux_registry.read().await.len();
    Json(json!({
        "ok": true,
        "port": state.port,
        "daemon_base_url": state.config.daemon.base_url,
        "configured_git_monitors": state.config.monitors.git.repos.len(),
        "configured_tmux_monitors": state.config.monitors.tmux.sessions.len(),
        "registered_tmux_sessions": registered,
    }))
}

async fn status(State(state): State<AppState>) -> impl IntoResponse {
    health(State(state)).await
}

async fn post_event(
    State(state): State<AppState>,
    Json(event): Json<IncomingEvent>,
) -> impl IntoResponse {
    let event = normalize_event(event);
    match state.router.dispatch(&event, state.discord.as_ref()).await {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(json!({"ok": true, "type": event.kind})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": error.to_string()})),
        )
            .into_response(),
    }
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

    let dispatch_result = match event_name {
        "issues" if action == "opened" => {
            let repo = payload
                .pointer("/repository/full_name")
                .and_then(Value::as_str)
                .unwrap_or("unknown/unknown")
                .to_string();
            let number = payload
                .pointer("/issue/number")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let title = payload
                .pointer("/issue/title")
                .and_then(Value::as_str)
                .unwrap_or("Untitled issue")
                .to_string();
            let event = IncomingEvent::github_issue_opened(repo, number, title, None);
            state.router.dispatch(&event, state.discord.as_ref()).await
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
            let transition = match action {
                "opened" => Some(("<new>".to_string(), "open".to_string())),
                "reopened" => Some(("closed".to_string(), "open".to_string())),
                "closed" if merged => Some(("open".to_string(), "merged".to_string())),
                "closed" => Some(("open".to_string(), "closed".to_string())),
                _ => None,
            };
            if let Some((old_status, new_status)) = transition {
                let event = IncomingEvent::github_pr_status_changed(
                    repo, number, title, old_status, new_status, url, None,
                );
                state.router.dispatch(&event, state.discord.as_ref()).await
            } else {
                return (
                    StatusCode::ACCEPTED,
                    Json(json!({"ok": true, "ignored": true, "reason": "unsupported pull_request action"})),
                )
                    .into_response();
            }
        }
        _ => {
            return (
                StatusCode::ACCEPTED,
                Json(json!({"ok": true, "ignored": true, "reason": "unsupported event"})),
            )
                .into_response();
        }
    };

    match dispatch_result {
        Ok(()) => (StatusCode::ACCEPTED, Json(json!({"ok": true}))).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": error.to_string()})),
        )
            .into_response(),
    }
}
