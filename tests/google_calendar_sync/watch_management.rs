use super::*;

#[tokio::test]
#[serial_test::serial(calendar_sync)]
async fn configured_source_creates_watch_with_callback_and_channel_token() {
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
        .expect("initial Calendar sync never started");
    state.release_initial.notify_one();
    timeout(Duration::from_secs(2), state.watch_requested.notified())
        .await
        .expect("configured Calendar source never created a watch channel");
    let watch = state
        .watch_body
        .lock()
        .expect("watch request lock")
        .clone()
        .expect("watch request body");
    assert_eq!(watch["type"], "web_hook");
    assert_eq!(
        watch["address"],
        "https://calendar.example.test/google/calendar"
    );
    assert_eq!(watch["token"], "test-channel-token");
    assert!(
        watch["id"].as_str().is_some_and(|id| !id.is_empty()),
        "watch channel ID must be generated"
    );

    daemon.stop().await;
    server.abort();
    let _ = server.await;
}

#[tokio::test]
#[serial_test::serial(calendar_sync)]
async fn past_expiration_renews_the_active_watch_immediately() {
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
    let expiration_ms = current_time_millis() - 1;
    seed_active_watch(&temp, expiration_ms);
    let config_path = write_calendar_config(&temp, api_addr, 0, 0);
    let (daemon, _listening) = spawn_daemon(&config_path, 0);

    timeout(Duration::from_secs(2), state.watch_requested.notified())
        .await
        .expect("expired active Calendar watch was not renewed");

    daemon.stop().await;
    server.abort();
    let _ = server.await;
}

#[tokio::test]
#[serial_test::serial(calendar_sync)]
async fn new_watch_sync_activates_channel_and_stops_previous_watch() {
    let temp = tempfile::tempdir().expect("temporary test directory");
    let (state, mut delivery_rx) = fake_state(false);
    state.stop_failures_remaining.store(100, Ordering::SeqCst);
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
    seed_active_watch(&temp, current_time_millis() - 1);
    let daemon_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reserve daemon port");
    let daemon_port = daemon_listener.local_addr().expect("daemon address").port();
    drop(daemon_listener);
    let config_path = write_calendar_config(&temp, api_addr, daemon_port, 0);
    let (daemon, listening) = spawn_daemon(&config_path, daemon_port);

    timeout(Duration::from_secs(2), state.watch_requested.notified())
        .await
        .expect("replacement Calendar watch was not created");
    timeout(Duration::from_secs(2), listening.notified())
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
    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{daemon_port}/google/calendar"))
        .headers(pending_calendar_headers(&channel_id, "sync", 1))
        .send()
        .await
        .expect("post replacement channel sync");
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    timeout(Duration::from_secs(2), state.stop_requested.notified())
        .await
        .expect("replacement sync never stopped the previous Calendar watch");
    let alert = timeout(Duration::from_secs(8), async {
        loop {
            let delivery = delivery_rx.recv().await.expect("fake sink channel closed");
            if delivery["content"]
                .as_str()
                .is_some_and(|content| content.contains("Calendar watch stop failed"))
            {
                return delivery;
            }
        }
    })
    .await
    .expect("failed stop did not emit a routed alert");
    assert!(
        alert["content"]
            .as_str()
            .is_some_and(|content| content.contains("Calendar watch stop failed"))
    );
    state.stop_failures_remaining.store(0, Ordering::SeqCst);
    daemon.stop().await;
    let requests_before_restart = state.stop_requests.load(Ordering::SeqCst);
    let persisted: serde_json::Value = serde_json::from_slice(
        &std::fs::read(temp.path().join("calendar-state.json"))
            .expect("read persisted Calendar state"),
    )
    .expect("parse persisted Calendar state");
    assert_eq!(persisted["retiring_watches"][0]["id"], "old-channel");

    let (daemon, listening) = spawn_daemon(&config_path, daemon_port);
    timeout(Duration::from_secs(2), listening.notified())
        .await
        .expect("restarted op_pi daemon never announced its listener");
    timeout(Duration::from_secs(4), state.stop_succeeded.notified())
        .await
        .expect("retiring watch stop was not retried after restart");
    assert_eq!(
        state.stop_requests.load(Ordering::SeqCst),
        requests_before_restart + 1
    );
    let stop = state
        .stop_body
        .lock()
        .expect("stop request lock")
        .clone()
        .expect("stop request body");
    assert_eq!(stop["id"], "old-channel");
    assert_eq!(stop["resourceId"], "old-resource");

    daemon.stop().await;
    server.abort();
    let _ = server.await;
}
