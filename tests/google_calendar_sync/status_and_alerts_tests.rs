use super::*;

#[tokio::test]
#[serial_test::serial(calendar_sync)]
async fn failed_watch_renewal_preserves_active_channel_and_delivers_alert() {
    let temp = tempfile::tempdir().expect("temporary test directory");
    let (mut state, mut delivery_rx) = fake_state(false);
    state.fail_watch = true;
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
        .with_state(state);
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("fake Calendar API server");
    });
    seed_active_watch(&temp, current_time_millis() + 3_600_000);
    let config_path = write_calendar_config(&temp, api_addr, 0, 7_200);
    let (daemon, _listening) = spawn_daemon(&config_path, 0);

    let alert = timeout(Duration::from_secs(8), delivery_rx.recv())
        .await
        .expect("watch renewal failure did not reach the configured sink")
        .expect("fake sink channel closed");
    let content = alert["content"].as_str().unwrap_or_default();
    assert!(
        content.contains("Calendar watch renewal failed"),
        "unexpected failure alert: {content}"
    );
    let persisted: serde_json::Value = serde_json::from_slice(
        &std::fs::read(temp.path().join("calendar-state.json"))
            .expect("read persisted Calendar state"),
    )
    .expect("parse persisted Calendar state");
    assert_eq!(persisted["active_watch"]["id"], "old-channel");

    daemon.stop().await;
    server.abort();
    let _ = server.await;
}

#[tokio::test]
#[serial_test::serial(calendar_sync)]
async fn failed_incremental_sync_degrades_health_and_delivers_alert() {
    let temp = tempfile::tempdir().expect("temporary test directory");
    seed_tracked_watch(&temp);
    let (state, mut delivery_rx) = fake_state(false);
    state
        .incremental_failures_remaining
        .store(100, Ordering::SeqCst);
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
    state.release_initial.notify_one();
    timeout(Duration::from_secs(2), listening.notified())
        .await
        .expect("op_pi daemon never announced its listener");
    let client = reqwest::Client::new();
    let callback = format!("http://127.0.0.1:{daemon_port}/google/calendar");
    let changed = client
        .post(&callback)
        .headers(calendar_headers())
        .send()
        .await
        .expect("post Calendar change notification");
    assert_eq!(changed.status(), StatusCode::ACCEPTED);

    let alert = timeout(Duration::from_secs(8), delivery_rx.recv())
        .await
        .expect("incremental sync failure did not reach the configured sink")
        .expect("fake sink channel closed");
    let content = alert["content"].as_str().unwrap_or_default();
    assert!(
        content.contains("Calendar incremental sync failed"),
        "unexpected sync failure alert: {content}"
    );
    assert!(!content.contains("initial-sync-token"));
    assert!(!content.contains("syncToken"));
    daemon.stop().await;
    let requests_before_restart = state.incremental_requests.load(Ordering::SeqCst);
    state
        .incremental_failures_remaining
        .store(0, Ordering::SeqCst);
    let (restarted, restarted_listening) = spawn_daemon(&config_path, daemon_port);
    timeout(Duration::from_secs(2), restarted_listening.notified())
        .await
        .expect("restarted op_pi daemon never announced its listener");
    timeout(
        Duration::from_secs(4),
        state.incremental_succeeded.notified(),
    )
    .await
    .expect("incremental sync did not resume after restart");
    assert_eq!(
        state.incremental_requests.load(Ordering::SeqCst),
        requests_before_restart + 1
    );
    let recovered_delivery = timeout(Duration::from_secs(8), async {
        loop {
            let delivery = delivery_rx.recv().await.expect("fake sink channel closed");
            if delivery["content"].as_str().is_some_and(|content| {
                content.contains("Created:")
                    || content.contains("Updated:")
                    || content.contains("Cancelled:")
            }) {
                return delivery;
            }
        }
    })
    .await
    .expect("recovered Calendar change did not reach the sink");
    assert!(
        recovered_delivery["content"]
            .as_str()
            .is_some_and(|content| {
                content.contains("Created:")
                    || content.contains("Updated:")
                    || content.contains("Cancelled:")
            })
    );
    let health = client
        .get(format!("http://127.0.0.1:{daemon_port}/health"))
        .send()
        .await
        .expect("request op_pi health")
        .error_for_status()
        .expect("successful health response")
        .json::<serde_json::Value>()
        .await
        .expect("decode health");
    assert_eq!(health["ok"], true);
    assert!(health["sources"]["google-calendar"]["details"]["last_error"].is_null());

    restarted.stop().await;
    server.abort();
    let _ = server.await;
}
