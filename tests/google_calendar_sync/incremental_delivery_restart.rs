use super::*;

#[tokio::test]
#[serial_test::serial(calendar_sync)]
async fn failed_sink_delivery_survives_daemon_restart_and_retries_same_outbox_event() {
    let temp = tempfile::tempdir().expect("temporary test directory");
    seed_tracked_watch(&temp);
    let (state, mut delivery_rx) = fake_state(false);
    state.sink_failures_remaining.store(100, Ordering::SeqCst);
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

    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{daemon_port}/google/calendar"))
        .headers(calendar_headers())
        .send()
        .await
        .expect("post Calendar notification");
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let failed_payload = timeout(Duration::from_secs(8), delivery_rx.recv())
        .await
        .expect("failed sink never received Calendar event")
        .expect("fake sink channel closed");
    daemon.stop().await;
    state.sink_failures_remaining.store(0, Ordering::SeqCst);
    let persisted_state: serde_json::Value = serde_json::from_slice(
        &std::fs::read(temp.path().join("calendar-state.json"))
            .expect("read Calendar state after failed delivery"),
    )
    .expect("parse Calendar state after failed delivery");
    assert!(
        persisted_state["outbox"]
            .as_array()
            .is_some_and(|outbox| !outbox.is_empty()),
        "failed delivery must remain in the durable outbox"
    );

    let daemon_listener = TcpListener::bind(("127.0.0.1", daemon_port))
        .await
        .expect("rebind daemon proxy port");
    let (restarted, restarted_listening) = spawn_daemon_with_proxy(&config_path, daemon_listener);
    timeout(TEST_EVENT_TIMEOUT, restarted_listening.notified())
        .await
        .expect("restarted op_pi daemon never announced its listener");
    let recovered_payload = timeout(Duration::from_secs(8), delivery_rx.recv())
        .await
        .expect("restarted daemon never retried Calendar outbox event")
        .expect("fake sink channel closed");
    assert_eq!(
        recovered_payload["content"], failed_payload["content"],
        "restart must retry the same persisted Calendar event"
    );

    restarted.stop().await;
    server.abort();
    let _ = server.await;
}
