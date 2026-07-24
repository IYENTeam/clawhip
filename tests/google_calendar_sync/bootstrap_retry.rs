use super::*;

#[tokio::test]
#[serial_test::serial(calendar_sync)]
async fn repaired_oauth_credentials_recover_without_daemon_restart() {
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
    let daemon_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reserve daemon port");
    let daemon_port = daemon_listener.local_addr().expect("daemon address").port();
    drop(daemon_listener);
    let config_path = write_calendar_config(&temp, api_addr, daemon_port, 86_400);
    let credentials_path = temp.path().join("oauth.json");
    let valid_credentials =
        std::fs::read(&credentials_path).expect("read valid credential fixture");
    std::fs::write(&credentials_path, b"{ invalid-json").expect("write invalid credential fixture");
    #[cfg(unix)]
    std::fs::set_permissions(&credentials_path, std::fs::Permissions::from_mode(0o600))
        .expect("secure invalid credential fixture");
    let (daemon, listening) = spawn_daemon(&config_path, daemon_port);

    timeout(Duration::from_secs(2), listening.notified())
        .await
        .expect("op_pi daemon never announced its listener");
    std::fs::write(&credentials_path, valid_credentials).expect("repair OAuth credentials");
    #[cfg(unix)]
    std::fs::set_permissions(&credentials_path, std::fs::Permissions::from_mode(0o600))
        .expect("secure repaired credential fixture");
    timeout(Duration::from_secs(4), state.initial_requested.notified())
        .await
        .expect("Calendar source did not retry repaired credentials");
    state.release_initial.notify_one();

    daemon.stop().await;
    server.abort();
    let _ = server.await;
}

#[tokio::test]
#[serial_test::serial(calendar_sync)]
async fn failed_initial_calendar_sync_retries_without_daemon_restart() {
    let temp = tempfile::tempdir().expect("temporary test directory");
    let (state, _delivery_rx) = fake_state(false);
    state.initial_failures_remaining.store(1, Ordering::SeqCst);
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
    let (daemon, _listening) = spawn_daemon(&config_path, daemon_port);

    timeout(Duration::from_secs(10), async {
        while state.initial_requests.load(Ordering::SeqCst) < 2 {
            state.initial_requested.notified().await;
        }
    })
    .await
    .expect("initial Calendar sync was not retried");
    state.release_initial.notify_one();
    timeout(Duration::from_secs(2), state.watch_requested.notified())
        .await
        .expect("recovered initial sync never reached watch creation");

    daemon.stop().await;
    server.abort();
    let _ = server.await;
}
