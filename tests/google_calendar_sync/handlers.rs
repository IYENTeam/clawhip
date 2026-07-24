use super::common::*;

pub(crate) async fn fake_oauth_token() -> Json<serde_json::Value> {
    Json(json!({
        "access_token": "test-access-token",
        "expires_in": 3600,
        "scope": "https://www.googleapis.com/auth/calendar.events.readonly",
        "token_type": "Bearer"
    }))
}

pub(crate) async fn fake_initial_events(
    State(state): State<FakeCalendarState>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Response {
    if query.contains_key("syncToken") {
        state.incremental_seen.store(true, Ordering::SeqCst);
        state.incremental_requests.fetch_add(1, Ordering::SeqCst);
        state.incremental_requested.notify_one();
        let fail_remaining = state
            .incremental_failures_remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok();
        if state.fail_incremental || fail_remaining {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": {"message": "sync unavailable"}})),
            )
                .into_response();
        }
        if state.expire_incremental {
            return (
                StatusCode::GONE,
                Json(json!({"error": {"code": 410, "message": "Sync token is no longer valid"}})),
            )
                .into_response();
        }
        state.incremental_succeeded.notify_one();
        return Json(json!({
            "items": [
                {
                    "id": "existing-event",
                    "status": "confirmed",
                    "summary": "Updated planning",
                    "created": "2026-07-20T01:00:00Z",
                    "updated": "2026-07-23T08:00:00Z",
                    "start": {"dateTime": "2026-07-24T10:00:00+09:00"},
                    "end": {"dateTime": "2026-07-24T11:00:00+09:00"},
                    "attendees": [{"email": "owner@example.com", "displayName": "Owner"}],
                    "hangoutLink": "https://meet.google.com/abc-defg-hij"
                },
                {
                    "id": "new-event",
                    "status": "confirmed",
                    "summary": "Created launch",
                    "created": "2026-07-23T08:01:00Z",
                    "updated": "2026-07-23T08:01:00Z",
                    "start": {"dateTime": "2026-07-25T10:00:00+09:00"},
                    "end": {"dateTime": "2026-07-25T11:00:00+09:00"}
                },
                {
                    "id": "cancelled-event",
                    "status": "cancelled",
                    "updated": "2026-07-23T08:02:00Z"
                }
            ],
            "nextSyncToken": "incremental-sync-token"
        }))
        .into_response();
    }
    if state.incremental_seen.load(Ordering::SeqCst) {
        state.recovery_requests.fetch_add(1, Ordering::SeqCst);
        state.recovery_full_requested.notify_one();
        if state
            .recovery_failures_remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": {"message": "recovery unavailable"}})),
            )
                .into_response();
        }
        state.recovery_succeeded.notify_one();
        return Json(json!({
            "items": [
                {
                    "id": "existing-event",
                    "status": "confirmed",
                    "summary": "Original planning",
                    "created": "2026-07-20T01:00:00Z",
                    "updated": "2026-07-20T01:00:00Z"
                },
                {
                    "id": "cancelled-event",
                    "status": "confirmed",
                    "summary": "Soon cancelled",
                    "created": "2026-07-20T02:00:00Z",
                    "updated": "2026-07-20T02:00:00Z"
                }
            ],
            "nextSyncToken": "recovered-sync-token"
        }))
        .into_response();
    }
    state.initial_requests.fetch_add(1, Ordering::SeqCst);
    state.initial_requested.notify_one();
    if state
        .initial_failures_remaining
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
            remaining.checked_sub(1)
        })
        .is_ok()
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "baseline unavailable"}})),
        )
            .into_response();
    }
    state.release_initial.notified().await;
    Json(json!({
        "items": [
            {
                "id": "existing-event",
                "status": "confirmed",
                "summary": "Original planning",
                "created": "2026-07-20T01:00:00Z",
                "updated": "2026-07-20T01:00:00Z"
            },
            {
                "id": "cancelled-event",
                "status": "confirmed",
                "summary": "Soon cancelled",
                "created": "2026-07-20T02:00:00Z",
                "updated": "2026-07-20T02:00:00Z"
            }
        ],
        "nextSyncToken": "initial-sync-token"
    }))
    .into_response()
}

pub(crate) async fn fake_watch(
    State(state): State<FakeCalendarState>,
    Json(payload): Json<serde_json::Value>,
) -> Response {
    *state.watch_body.lock().expect("watch request lock") = Some(payload.clone());
    state.watch_requested.notify_one();
    if state.fail_watch {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "watch unavailable"}})),
        )
            .into_response();
    }
    if let Some(callback_url) = state.callback_during_watch.as_deref() {
        let channel_id = payload["id"].as_str().unwrap_or_default();
        let channel_token = payload["token"].as_str().unwrap_or_default();
        let status = reqwest::Client::new()
            .post(callback_url)
            .headers(pending_calendar_headers(channel_id, "sync", 1))
            .header("x-goog-channel-token", channel_token)
            .send()
            .await
            .map(|response| response.status().as_u16())
            .unwrap_or_default();
        *state.callback_status.lock().expect("callback status lock") = Some(status);
        state.callback_completed.notify_one();
    }
    Json(json!({
        "kind": "api#channel",
        "id": payload["id"],
        "resourceId": "resource-new",
        "resourceUri": "https://www.googleapis.com/calendar/v3/calendars/primary/events",
        "expiration": "1785400000000"
    }))
    .into_response()
}

pub(crate) async fn fake_stop(
    State(state): State<FakeCalendarState>,
    Json(payload): Json<serde_json::Value>,
) -> StatusCode {
    *state.stop_body.lock().expect("stop request lock") = Some(payload);
    state.stop_requests.fetch_add(1, Ordering::SeqCst);
    state.stop_requested.notify_one();
    if state
        .stop_failures_remaining
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
            remaining.checked_sub(1)
        })
        .is_ok()
    {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    state.stop_succeeded.notify_one();
    StatusCode::NO_CONTENT
}

pub(crate) async fn fake_sink(
    State(state): State<FakeCalendarState>,
    Json(payload): Json<serde_json::Value>,
) -> StatusCode {
    state
        .delivery_tx
        .send(payload)
        .expect("fake delivery receiver");
    if state
        .sink_failures_remaining
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
            remaining.checked_sub(1)
        })
        .is_ok()
    {
        StatusCode::INTERNAL_SERVER_ERROR
    } else {
        StatusCode::NO_CONTENT
    }
}
