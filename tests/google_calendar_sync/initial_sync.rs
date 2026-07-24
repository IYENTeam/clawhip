use super::*;

#[tokio::test]
#[serial_test::serial(calendar_sync)]
async fn configured_daemon_establishes_initial_calendar_sync_cursor() {
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
        .route("/sink", post(fake_sink))
        .with_state(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("fake Calendar API server");
    });

    let config_path = write_calendar_config(&temp, api_addr, 0, 86_400);
    let (daemon, _listening) = spawn_daemon(&config_path, 0);

    timeout(Duration::from_secs(2), state.initial_requested.notified())
        .await
        .expect("configured daemon never requested the initial Calendar events page");
    state.release_initial.notify_one();

    daemon.stop().await;
    server.abort();
    let _ = server.await;
}

#[tokio::test]
#[serial_test::serial(calendar_sync)]
async fn accepted_calendar_change_triggers_incremental_sync_with_saved_token() {
    let temp = tempfile::tempdir().expect("temporary test directory");
    seed_tracked_watch(&temp);
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
    drop(daemon_listener);
    let config_path = write_calendar_config(&temp, api_addr, daemon_port, 86_400);
    let (daemon, listening) = spawn_daemon(&config_path, daemon_port);

    timeout(Duration::from_secs(2), state.initial_requested.notified())
        .await
        .expect("initial Calendar sync never started");
    timeout(Duration::from_secs(2), listening.notified())
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

    timeout(
        Duration::from_secs(2),
        state.incremental_requested.notified(),
    )
    .await
    .expect("accepted Calendar notification never triggered incremental sync");

    daemon.stop().await;
    server.abort();
    let _ = server.await;
}

#[tokio::test]
#[serial_test::serial(calendar_sync)]
async fn expired_incremental_cursor_triggers_fresh_full_sync() {
    let temp = tempfile::tempdir().expect("temporary test directory");
    seed_tracked_watch(&temp);
    let (state, _delivery_rx) = fake_state(true);
    state.recovery_failures_remaining.store(1, Ordering::SeqCst);
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
    drop(daemon_listener);
    let config_path = write_calendar_config(&temp, api_addr, daemon_port, 86_400);
    let (daemon, listening) = spawn_daemon(&config_path, daemon_port);

    timeout(Duration::from_secs(2), state.initial_requested.notified())
        .await
        .expect("initial Calendar sync never started");
    timeout(Duration::from_secs(2), listening.notified())
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

    timeout(Duration::from_secs(4), state.recovery_succeeded.notified())
        .await
        .expect("HTTP 410 recovery did not retry to success");
    assert_eq!(state.recovery_requests.load(Ordering::SeqCst), 2);

    daemon.stop().await;
    server.abort();
    let _ = server.await;
}
