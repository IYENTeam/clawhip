use super::*;

#[tokio::test]
#[serial_test::serial(calendar_sync)]
async fn status_reports_calendar_cursor_channel_expiration_and_last_notification() {
    let temp = tempfile::tempdir().expect("temporary test directory");
    let (state, _delivery_rx) = fake_state(false);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake Calendar API");
    let api_addr = listener.local_addr().expect("fake API address");
    let app = Router::new()
        .route("/token", post(fake_oauth_token))
        .route(
            "/calendar/v3/calendars/primary/events",
            get(fake_initial_events),
        )
        .route(
            "/calendar/v3/calendars/primary/events/watch",
            post(fake_watch),
        )
        .route("/calendar/v3/channels/stop", post(fake_stop))
        .route("/sink", post(fake_sink))
        .with_state(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("fake Calendar API server");
    });
    let daemon_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reserve daemon port");
    let daemon_port = daemon_listener.local_addr().expect("daemon address").port();
    let config_path = write_calendar_config(&temp, api_addr, daemon_port, 86_400);
    let (daemon, listening) = spawn_daemon_with_proxy(&config_path, daemon_listener);

    timeout(TEST_EVENT_TIMEOUT, state.initial_requested.notified())
        .await
        .expect("initial Calendar sync never started");
    state.release_initial.notify_one();
    timeout(TEST_EVENT_TIMEOUT, state.watch_requested.notified())
        .await
        .expect("Calendar watch was not created");
    timeout(TEST_EVENT_TIMEOUT, listening.notified())
        .await
        .expect("op_pi daemon never announced its listener");
    let channel_id = state
        .watch_body
        .lock()
        .expect("watch request lock")
        .as_ref()
        .and_then(|watch| watch["id"].as_str())
        .expect("new channel id")
        .to_string();
    let client = reqwest::Client::new();
    let callback = format!("http://127.0.0.1:{daemon_port}/google/calendar");
    let sync = client
        .post(&callback)
        .headers(pending_calendar_headers(&channel_id, "sync", 1))
        .send()
        .await
        .expect("post channel sync");
    assert_eq!(sync.status(), StatusCode::ACCEPTED);
    timeout(TEST_EVENT_TIMEOUT, state.incremental_succeeded.notified())
        .await
        .expect("channel sync never completed successfully");

    let status = client
        .get(format!("http://127.0.0.1:{daemon_port}/api/status"))
        .send()
        .await
        .expect("request op_pi status")
        .error_for_status()
        .expect("successful op_pi status")
        .json::<serde_json::Value>()
        .await
        .expect("decode op_pi status");
    let details = &status["sources"]["google-calendar"]["details"];
    assert_eq!(details["cursor_present"], true);
    assert_eq!(details["active_channel_id"], channel_id);
    assert_eq!(details["active_expiration_ms"], TEST_WATCH_EXPIRATION_MS);
    assert_eq!(details["last_message_number"], 1);
    assert!(
        details["last_notification_at"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(
        details["last_successful_sync_at"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(details["last_error"].is_null());

    daemon.stop().await;
    server.abort();
    let _ = server.await;
}
