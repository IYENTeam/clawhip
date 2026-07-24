use std::time::Duration;

use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use tokio::net::TcpListener;
use tokio::time::timeout;

use super::CalendarApi;
use crate::calendar::credentials::AuthorizedUserCredentials;
use crate::calendar::state::WatchChannel;

#[tokio::test]
async fn calendar_http_requests_have_a_bounded_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind hanging Calendar server");
    let address = listener.local_addr().expect("hanging server address");
    let server = tokio::spawn(async move {
        let (_socket, _peer) = listener.accept().await.expect("accept OAuth request");
        std::future::pending::<()>().await;
    });
    let credentials: AuthorizedUserCredentials = serde_json::from_value(serde_json::json!({
        "client_id": "client",
        "client_secret": "secret",
        "refresh_token": "refresh",
        "token_uri": format!("http://{address}/token")
    }))
    .expect("OAuth credential fixture");
    let api = CalendarApi::new(
        &format!("http://{address}/calendar/v3"),
        "primary",
        credentials,
    )
    .expect("Calendar API client");

    let request = timeout(Duration::from_millis(500), api.establish_initial_sync())
        .await
        .expect("Calendar client request did not enforce its timeout");
    assert!(request.is_err());

    server.abort();
    let _ = server.await;
}

async fn stop_watch_with_status(status: StatusCode) -> crate::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Calendar API");
    let address = listener.local_addr().expect("fake Calendar API address");
    let app = Router::new()
        .route(
            "/token",
            post(|| async {
                Json(serde_json::json!({
                    "access_token": "token",
                    "scope": "https://www.googleapis.com/auth/calendar.events.readonly",
                    "expires_in": 3600,
                }))
            }),
        )
        .route(
            "/calendar/v3/channels/stop",
            post(move || async move { status }),
        );
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve fake Calendar API");
    });
    let credentials: AuthorizedUserCredentials = serde_json::from_value(serde_json::json!({
        "client_id": "client",
        "client_secret": "secret",
        "refresh_token": "refresh",
        "token_uri": format!("http://{address}/token")
    }))
    .expect("OAuth credential fixture");
    let api = CalendarApi::new(
        &format!("http://{address}/calendar/v3"),
        "primary",
        credentials,
    )
    .expect("Calendar API client");
    let result = api
        .stop_watch(&WatchChannel {
            id: "channel".into(),
            resource_id: "resource".into(),
            resource_uri: "https://www.googleapis.com/calendar/v3/calendars/primary/events".into(),
            expiration_ms: 0,
            activation_deadline_ms: None,
        })
        .await;

    server.abort();
    let _ = server.await;
    result
}

#[tokio::test]
async fn stopping_an_already_retired_watch_is_successful() {
    stop_watch_with_status(StatusCode::NOT_FOUND)
        .await
        .expect("404 stop response retires the watch");
    stop_watch_with_status(StatusCode::GONE)
        .await
        .expect("410 stop response retires the watch");
    assert!(
        stop_watch_with_status(StatusCode::INTERNAL_SERVER_ERROR)
            .await
            .is_err()
    );
}
