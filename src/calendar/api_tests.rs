use std::time::Duration;

use tokio::net::TcpListener;
use tokio::time::timeout;

use super::CalendarApi;
use crate::calendar::credentials::AuthorizedUserCredentials;

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
