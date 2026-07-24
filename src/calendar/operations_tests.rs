use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::extract::Json;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Router, response::IntoResponse};
use serde_json::{Value, json};
use tokio::net::TcpListener;

use super::*;
use crate::calendar::credentials::AuthorizedUserCredentials;
use crate::calendar::state::WatchChannel;
use crate::source::new_shared_source_health;

fn credentials(token_uri: String) -> AuthorizedUserCredentials {
    serde_json::from_value(json!({
        "client_id": "client",
        "client_secret": "secret",
        "refresh_token": "refresh",
        "token_uri": token_uri,
    }))
    .expect("OAuth credential fixture")
}

fn watch(id: &str) -> WatchChannel {
    WatchChannel {
        id: id.into(),
        resource_id: format!("resource-{id}"),
        resource_uri: "https://www.googleapis.com/calendar/v3/calendars/primary/events".into(),
        expiration_ms: 1,
        activation_deadline_ms: None,
    }
}

async fn oauth_token() -> Json<Value> {
    Json(json!({
        "access_token": "token",
        "scope": "https://www.googleapis.com/auth/calendar.events.readonly",
        "expires_in": 3600,
    }))
}

#[tokio::test]
async fn repeated_active_failure_queues_once_and_recovery_allows_a_new_alert() {
    let temp = tempfile::tempdir().expect("temporary state directory");
    let path = temp.path().join("calendar-state.json");
    let health = new_shared_source_health();
    let mut state = CalendarState::default();
    let failure = CalendarFailure {
        domain: FailureDomain::Sync,
        kind: "calendar.sync.failed",
        summary: "Calendar incremental sync failed",
        code: "calendar_incremental_sync_failed",
    };

    for _ in 0..100 {
        record_failure(&health, "google-calendar", &mut state, &path, failure)
            .await
            .expect("record repeated failure");
    }
    assert_eq!(state.outbox.len(), 1);

    state.sync_error = None;
    record_failure(&health, "google-calendar", &mut state, &path, failure)
        .await
        .expect("record failure after recovery");
    assert_eq!(state.outbox.len(), 2);
}

#[tokio::test]
async fn independent_watch_failures_are_deduplicated_and_recover_independently() {
    let temp = tempfile::tempdir().expect("temporary state directory");
    let path = temp.path().join("calendar-state.json");
    let health = new_shared_source_health();
    let mut state = CalendarState::default();
    let renewal = watch_failure(
        "calendar.watch.renewal-failed",
        "Calendar watch renewal failed",
        WATCH_RENEWAL_FAILED,
    );
    let stop = watch_failure(
        "calendar.watch.stop-failed",
        "Calendar watch stop failed",
        WATCH_STOP_FAILED,
    );

    record_failure(&health, "google-calendar", &mut state, &path, renewal)
        .await
        .expect("record renewal failure");
    record_failure(&health, "google-calendar", &mut state, &path, stop)
        .await
        .expect("record stop failure");
    let mut state = CalendarState::load(&path).expect("reload persisted watch failures");
    record_failure(&health, "google-calendar", &mut state, &path, renewal)
        .await
        .expect("repeat renewal failure");
    record_failure(&health, "google-calendar", &mut state, &path, stop)
        .await
        .expect("repeat stop failure");
    assert_eq!(state.outbox.len(), 2);

    assert!(state.clear_watch_failure(WATCH_RENEWAL_FAILED));
    record_failure(&health, "google-calendar", &mut state, &path, renewal)
        .await
        .expect("record renewal failure after recovery");
    assert_eq!(state.outbox.len(), 3);
    assert!(state.has_watch_failures());
}

#[test]
fn retry_delay_increases_and_is_bounded() {
    assert!(retry_delay(0) < retry_delay(1));
    assert!(retry_delay(1) < retry_delay(2));
    assert_eq!(retry_delay(6), retry_delay(u32::MAX));
    assert_eq!(retry_delay(u32::MAX).as_secs(), RETRY_MAX_SECS);
}

#[tokio::test]
async fn failed_retirement_does_not_block_later_retirements_and_clears_only_stop_failures() {
    let blocked_attempts = Arc::new(AtomicUsize::new(0));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Calendar API");
    let address = listener.local_addr().expect("fake Calendar API address");
    let app = Router::new().route("/token", post(oauth_token)).route(
        "/calendar/v3/channels/stop",
        post({
            let blocked_attempts = Arc::clone(&blocked_attempts);
            move |Json(payload): Json<Value>| {
                let blocked_attempts = Arc::clone(&blocked_attempts);
                async move {
                    if payload["id"] == "blocked"
                        && blocked_attempts.fetch_add(1, Ordering::SeqCst) == 0
                    {
                        StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    } else {
                        StatusCode::NO_CONTENT.into_response()
                    }
                }
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Calendar API");
    });
    let api = CalendarApi::new(
        &format!("http://{address}/calendar/v3"),
        "primary",
        credentials(format!("http://{address}/token")),
    )
    .expect("Calendar API client");
    let temp = tempfile::tempdir().expect("temporary state directory");
    let path = temp.path().join("calendar-state.json");
    let mut state = CalendarState::default();
    state.retiring_watches = vec![watch("blocked"), watch("later")];
    state.watch_error = Some(WATCH_STOP_FAILED.into());

    assert!(
        stop_retiring_watches(&api, &mut state, &path)
            .await
            .is_err()
    );
    assert_eq!(state.retiring_watches.len(), 1);
    assert_eq!(state.retiring_watches[0].id, "blocked");
    assert_eq!(state.watch_error.as_deref(), Some(WATCH_STOP_FAILED));

    stop_retiring_watches(&api, &mut state, &path)
        .await
        .expect("retry blocked retirement");
    assert!(state.retiring_watches.is_empty());
    assert!(state.watch_error.is_none());
    assert_eq!(blocked_attempts.load(Ordering::SeqCst), 2);
    let persisted = CalendarState::load(&path).expect("reload retirement state");
    assert!(persisted.retiring_watches.is_empty());
    assert!(persisted.watch_error.is_none());

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn successful_retirement_does_not_clear_a_renewal_failure() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Calendar API");
    let address = listener.local_addr().expect("fake Calendar API address");
    let app = Router::new().route("/token", post(oauth_token)).route(
        "/calendar/v3/channels/stop",
        post(|| async { StatusCode::NO_CONTENT }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Calendar API");
    });
    let api = CalendarApi::new(
        &format!("http://{address}/calendar/v3"),
        "primary",
        credentials(format!("http://{address}/token")),
    )
    .expect("Calendar API client");
    let temp = tempfile::tempdir().expect("temporary state directory");
    let path = temp.path().join("calendar-state.json");
    let mut state = CalendarState::default();
    state.retiring_watches = vec![watch("retiring")];
    state.watch_error = Some(WATCH_RENEWAL_FAILED.into());

    stop_retiring_watches(&api, &mut state, &path)
        .await
        .expect("retire watch");
    assert_eq!(state.watch_error.as_deref(), Some(WATCH_RENEWAL_FAILED));

    server.abort();
    let _ = server.await;
}
