use super::*;

#[tokio::test]
#[serial_test::serial(calendar_sync)]
async fn incremental_calendar_changes_emit_created_updated_and_cancelled_deliveries() {
    let temp = tempfile::tempdir().expect("temporary test directory");
    seed_tracked_watch(&temp);
    let (state, mut delivery_rx) = fake_state(false);
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

    timeout(Duration::from_secs(10), state.initial_requested.notified())
        .await
        .expect("initial Calendar sync never started");
    timeout(TEST_EVENT_TIMEOUT, listening.notified())
        .await
        .expect("op_pi daemon never announced its listener");
    state.release_initial.notify_one();

    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{daemon_port}/google/calendar"))
        .headers(calendar_headers())
        .send()
        .await
        .expect("post Calendar notification");
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let mut contents = Vec::new();
    while !(contents
        .iter()
        .any(|content: &String| content.contains("Created"))
        && contents.iter().any(|content| content.contains("Updated"))
        && contents.iter().any(|content| content.contains("Cancelled")))
    {
        let payload = timeout(Duration::from_secs(8), delivery_rx.recv())
            .await
            .expect("typed Calendar event was not delivered")
            .expect("fake sink channel closed");
        contents.push(
            payload
                .get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
        );
    }
    assert!(
        contents.iter().any(|content| content.contains("Created")),
        "created delivery missing: {contents:?}"
    );
    assert!(
        contents.iter().any(|content| content.contains("Updated")),
        "updated delivery missing: {contents:?}"
    );
    assert!(
        contents.iter().any(|content| content.contains("Cancelled")),
        "cancelled delivery missing: {contents:?}"
    );

    daemon.stop().await;
    server.abort();
    let _ = server.await;
}
#[tokio::test]
#[serial_test::serial(calendar_sync)]
async fn duplicate_calendar_message_number_does_not_repeat_incremental_sync() {
    let temp = tempfile::tempdir().expect("temporary test directory");
    seed_tracked_watch(&temp);
    let (state, mut delivery_rx) = fake_state(false);
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
    timeout(TEST_EVENT_TIMEOUT, listening.notified())
        .await
        .expect("op_pi daemon never announced its listener");
    state.release_initial.notify_one();

    let client = reqwest::Client::new();
    let callback = format!("http://127.0.0.1:{daemon_port}/google/calendar");
    let first = client
        .post(&callback)
        .headers(calendar_headers())
        .send()
        .await
        .expect("post first Calendar notification");
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    timeout(TEST_EVENT_TIMEOUT, state.incremental_requested.notified())
        .await
        .expect("first Calendar notification never triggered sync");
    timeout(Duration::from_secs(8), delivery_rx.recv())
        .await
        .expect("first Calendar change never reached the sink")
        .expect("fake sink channel closed");

    let duplicate = client
        .post(&callback)
        .headers(calendar_headers())
        .send()
        .await
        .expect("post duplicate Calendar notification");
    assert_eq!(duplicate.status(), StatusCode::ACCEPTED);
    let barrier = client
        .post(&callback)
        .headers(calendar_headers_for("channel-1", "exists", 3))
        .send()
        .await
        .expect("post Calendar synchronization barrier");
    assert_eq!(barrier.status(), StatusCode::ACCEPTED);
    timeout(TEST_EVENT_TIMEOUT, state.incremental_requested.notified())
        .await
        .expect("barrier Calendar notification never triggered sync");
    assert_eq!(
        state.incremental_requests.load(Ordering::SeqCst),
        2,
        "duplicate message number triggered another incremental sync"
    );

    daemon.stop().await;
    let daemon_listener = TcpListener::bind(("127.0.0.1", daemon_port))
        .await
        .expect("rebind daemon proxy port");
    let (restarted, restarted_listening) = spawn_daemon_with_proxy(&config_path, daemon_listener);
    timeout(TEST_EVENT_TIMEOUT, restarted_listening.notified())
        .await
        .expect("restarted op_pi daemon never announced its listener");
    let after_restart = client
        .post(&callback)
        .headers(calendar_headers_for("channel-1", "exists", 3))
        .send()
        .await
        .expect("post duplicate after daemon restart");
    assert_eq!(after_restart.status(), StatusCode::ACCEPTED);
    let restart_barrier = client
        .post(&callback)
        .headers(calendar_headers_for("channel-1", "exists", 4))
        .send()
        .await
        .expect("post restart Calendar synchronization barrier");
    assert_eq!(restart_barrier.status(), StatusCode::ACCEPTED);
    timeout(TEST_EVENT_TIMEOUT, state.incremental_requested.notified())
        .await
        .expect("restart barrier Calendar notification never triggered sync");
    assert_eq!(
        state.incremental_requests.load(Ordering::SeqCst),
        3,
        "persisted duplicate message number triggered sync after restart"
    );

    restarted.stop().await;
    server.abort();
    let _ = server.await;
}
