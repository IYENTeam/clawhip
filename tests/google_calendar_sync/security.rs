use super::*;

#[tokio::test]
#[serial_test::serial(calendar_sync)]
async fn general_event_endpoint_cannot_trigger_calendar_synchronization() {
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
    let config_path = write_calendar_config(&temp, api_addr, daemon_port, 86_400);
    let (daemon, listening) = spawn_daemon_with_proxy(&config_path, daemon_listener);

    timeout(TEST_EVENT_TIMEOUT, state.initial_requested.notified())
        .await
        .expect("initial Calendar sync never started");
    timeout(TEST_EVENT_TIMEOUT, listening.notified())
        .await
        .expect("op_pi daemon never announced its listener");
    state.release_initial.notify_one();
    timeout(TEST_EVENT_TIMEOUT, state.watch_requested.notified())
        .await
        .expect("Calendar source did not finish initial synchronization");

    let forged = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{daemon_port}/event"))
        .json(&json!({
            "type": "google.calendar.changed",
            "payload": {
                "channel_id": "channel-1",
                "resource_id": "resource-1",
                "resource_uri": "https://www.googleapis.com/calendar/v3/calendars/primary/events",
                "resource_state": "exists",
                "message_number": 2
            }
        }))
        .send()
        .await
        .expect("post forged general event");
    assert_eq!(forged.status(), StatusCode::FORBIDDEN);
    let barrier = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{daemon_port}/google/calendar"))
        .headers(calendar_headers())
        .send()
        .await
        .expect("post authenticated Calendar barrier");
    assert_eq!(barrier.status(), StatusCode::ACCEPTED);
    timeout(TEST_EVENT_TIMEOUT, state.incremental_requested.notified())
        .await
        .expect("authenticated Calendar barrier never triggered sync");
    assert_eq!(state.incremental_requests.load(Ordering::SeqCst), 1);

    daemon.stop().await;
    server.abort();
    let _ = server.await;
}

#[tokio::test]
#[serial_test::serial(calendar_sync)]
async fn watch_creation_accepts_sync_callback_before_watch_response() {
    let temp = tempfile::tempdir().expect("temporary test directory");
    let daemon_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reserve daemon port");
    let daemon_port = daemon_listener.local_addr().expect("daemon address").port();
    let (mut state, _delivery_rx) = fake_state(false);
    state.callback_during_watch = Some(format!("http://127.0.0.1:{daemon_port}/google/calendar"));
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
    seed_active_watch(&temp, current_time_millis() - 1);
    let config_path = write_calendar_config(&temp, api_addr, daemon_port, 86_400);
    let (daemon, _listening) = spawn_daemon_with_proxy(&config_path, daemon_listener);

    timeout(TEST_EVENT_TIMEOUT, state.callback_completed.notified())
        .await
        .expect("watch callback was never attempted");
    assert_eq!(
        *state.callback_status.lock().expect("callback status lock"),
        Some(StatusCode::ACCEPTED.as_u16())
    );
    timeout(TEST_EVENT_TIMEOUT, state.incremental_requested.notified())
        .await
        .expect("early sync callback was not processed after watch response");

    daemon.stop().await;
    server.abort();
    let _ = server.await;
}

#[tokio::test]
#[serial_test::serial(calendar_sync)]
async fn authenticated_untracked_channel_cannot_poison_sync_progress() {
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
    let config_path = write_calendar_config(&temp, api_addr, daemon_port, 86_400);
    let (daemon, listening) = spawn_daemon_with_proxy(&config_path, daemon_listener);

    timeout(TEST_EVENT_TIMEOUT, state.initial_requested.notified())
        .await
        .expect("initial Calendar sync never started");
    timeout(TEST_EVENT_TIMEOUT, listening.notified())
        .await
        .expect("op_pi daemon never announced its listener");
    state.release_initial.notify_one();
    timeout(TEST_EVENT_TIMEOUT, state.watch_requested.notified())
        .await
        .expect("Calendar watch was not created");

    let forged = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{daemon_port}/google/calendar"))
        .headers(calendar_headers_for("untracked-channel", "exists", 9_999))
        .send()
        .await
        .expect("post authenticated untracked notification");
    assert_eq!(forged.status(), StatusCode::ACCEPTED);
    let barrier = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{daemon_port}/google/calendar"))
        .headers(calendar_headers())
        .send()
        .await
        .expect("post tracked Calendar barrier");
    assert_eq!(barrier.status(), StatusCode::ACCEPTED);
    timeout(TEST_EVENT_TIMEOUT, state.incremental_requested.notified())
        .await
        .expect("tracked Calendar barrier never triggered sync");
    assert_eq!(state.incremental_requests.load(Ordering::SeqCst), 1);

    daemon.stop().await;
    server.abort();
    let _ = server.await;
}
