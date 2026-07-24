use std::path::Path;
use std::time::Duration;

use crate::Result;
use crate::calendar::api::CalendarApi;
use crate::calendar::state::CalendarState;
use crate::calendar::watch::{now_millis, renewal_delay, renewal_due, requested_expiration_ms};
use crate::config::GoogleCalendarConfig;
use crate::events::IncomingEvent;
use crate::source::{
    SharedSourceHealth, mark_source_details, mark_source_error, mark_source_success,
};
use serde_json::json;

const ACTIVATION_WINDOW_MS: i64 = 5 * 60 * 1000;
const RETRY_BASE_SECS: u64 = 1;
const RETRY_MAX_SECS: u64 = 60;
const WATCH_RENEWAL_FAILED: &str = "calendar_watch_renewal_failed";
const WATCH_STOP_FAILED: &str = "calendar_watch_stop_failed";
pub(crate) const WATCH_ACTIVATION_TIMED_OUT: &str = "calendar_watch_activation_timed_out";

#[cfg(test)]
#[path = "operations_tests.rs"]
mod tests;

#[derive(Clone, Copy)]
pub struct CalendarFailure<'a> {
    pub domain: FailureDomain,
    pub kind: &'a str,
    pub summary: &'a str,
    pub code: &'a str,
}

#[derive(Clone, Copy)]
pub enum FailureDomain {
    Sync,
    Watch,
}

pub enum Renewal {
    None,
    ActivationTimedOut,
}

pub fn retry_delay(attempt: u32) -> Duration {
    let seconds = RETRY_BASE_SECS.saturating_mul(1_u64 << attempt.min(6));
    Duration::from_secs(seconds.min(RETRY_MAX_SECS))
}

pub async fn publish_status(health: &SharedSourceHealth, source_name: &str, state: &CalendarState) {
    mark_source_details(health, source_name, json!({
        "cursor_present": state.next_sync_token.is_some(),
        "active_channel_id": state.active_watch.as_ref().map(|watch| &watch.id),
        "active_expiration_ms": state.active_watch.as_ref().map(|watch| watch.expiration_ms),
        "pending_channel_id": state.pending_watch.as_ref().map(|watch| &watch.id),
        "pending_expiration_ms": state.pending_watch.as_ref().map(|watch| watch.expiration_ms),
        "retiring_channel_ids": state.retiring_watches.iter().map(|watch| &watch.id).collect::<Vec<_>>(),
        "outbox_depth": state.outbox.len(),
        "last_notification_at": state.last_notification_at,
        "last_successful_sync_at": state.last_successful_sync_at,
        "last_message_number": state.last_message_number,
        "sync_error": state.sync_error,
        "watch_error": state.watch_error,
        "last_error": state.sync_error.as_deref().or(state.watch_error.as_deref()),
    })).await;
}

pub async fn refresh_health(health: &SharedSourceHealth, source: &str, state: &CalendarState) {
    if let Some(error) = state.sync_error.as_deref().or(state.watch_error.as_deref()) {
        mark_source_error(health, source, error).await;
    } else {
        mark_source_success(health, source).await;
    }
    publish_status(health, source, state).await;
}

pub async fn record_failure(
    health: &SharedSourceHealth,
    source: &str,
    state: &mut CalendarState,
    path: &Path,
    failure: CalendarFailure<'_>,
) -> Result<()> {
    let is_new_failure = match failure.domain {
        FailureDomain::Sync => {
            let is_new_failure = state.sync_error.as_deref() != Some(failure.code);
            state.sync_error = Some(failure.code.to_string());
            is_new_failure
        }
        FailureDomain::Watch => state.record_watch_failure(failure.code),
    };
    if is_new_failure {
        state.queue(vec![IncomingEvent {
            kind: failure.kind.to_string(),
            channel: None,
            mention: None,
            format: None,
            template: None,
            payload: json!({"summary": failure.summary, "error": failure.code}),
        }]);
    }
    state.save(path)?;
    refresh_health(health, source, state).await;
    Ok(())
}

pub async fn renew_watch_if_due(
    api: &CalendarApi,
    calendar: &GoogleCalendarConfig,
    state: &mut CalendarState,
    path: &Path,
) -> Result<Renewal> {
    let (Some(callback), Some(token)) = (
        calendar.callback_url.as_deref(),
        calendar.channel_token.as_deref(),
    ) else {
        return Ok(Renewal::None);
    };
    let now = now_millis()?;
    let timed_out = state.pending_watch.as_ref().is_some_and(|watch| {
        watch
            .activation_deadline_ms
            .is_some_and(|deadline| deadline <= now)
    });
    if timed_out {
        if let Some(watch) = state.pending_watch.take() {
            state.retiring_watches.push(watch);
        }
        state.save(path)?;
    }
    if renewal_due(
        state.active_watch.as_ref(),
        state.pending_watch.as_ref(),
        now,
        calendar.renewal_margin_secs,
    ) {
        let mut watch = api
            .create_watch(callback, token, requested_expiration_ms(now))
            .await?;
        watch.activation_deadline_ms = Some(
            watch
                .expiration_ms
                .min(now.saturating_add(ACTIVATION_WINDOW_MS)),
        );
        state.pending_watch = Some(watch);
        clear_watch_error(state, WATCH_RENEWAL_FAILED);
        state.save(path)?;
    }
    Ok(if timed_out {
        Renewal::ActivationTimedOut
    } else {
        Renewal::None
    })
}

pub async fn stop_retiring_watches(
    api: &CalendarApi,
    state: &mut CalendarState,
    path: &Path,
) -> Result<()> {
    let mut index = 0;
    let mut retired_any = false;
    let mut first_error = None;
    while let Some(watch) = state.retiring_watches.get(index).cloned() {
        match api.stop_watch(&watch).await {
            Ok(()) => {
                state.retiring_watches.remove(index);
                retired_any = true;
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
                index += 1;
            }
        }
    }
    if retired_any {
        if first_error.is_none() {
            clear_watch_error(state, WATCH_STOP_FAILED);
        }
        state.save(path)?;
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

pub fn retire_active_watch(state: &mut CalendarState) {
    if let Some(watch) = state.active_watch.take() {
        state.retiring_watches.push(watch);
    }
}

pub async fn watch_cycle(
    api: &CalendarApi,
    calendar: &GoogleCalendarConfig,
    state: &mut CalendarState,
    path: &Path,
    health: &SharedSourceHealth,
    source: &str,
) -> Result<Duration> {
    match renew_watch_if_due(api, calendar, state, path).await {
        Ok(Renewal::ActivationTimedOut) => {
            record_failure(
                health,
                source,
                state,
                path,
                watch_failure(
                    "calendar.watch.activation-timed-out",
                    "Calendar watch activation timed out",
                    WATCH_ACTIVATION_TIMED_OUT,
                ),
            )
            .await?
        }
        Ok(Renewal::None) => {}
        Err(_) => {
            record_failure(
                health,
                source,
                state,
                path,
                watch_failure(
                    "calendar.watch.renewal-failed",
                    "Calendar watch renewal failed",
                    WATCH_RENEWAL_FAILED,
                ),
            )
            .await?;
        }
    }
    if !state.retiring_watches.is_empty() && stop_retiring_watches(api, state, path).await.is_err()
    {
        record_failure(
            health,
            source,
            state,
            path,
            watch_failure(
                "calendar.watch.stop-failed",
                "Calendar watch stop failed",
                WATCH_STOP_FAILED,
            ),
        )
        .await?;
    }
    refresh_health(health, source, state).await;
    Ok(renewal_delay(
        state.active_watch.as_ref(),
        state.pending_watch.as_ref(),
        now_millis()?,
        calendar.renewal_margin_secs,
    ))
}

fn clear_watch_error(state: &mut CalendarState, error: &str) {
    state.clear_watch_failure(error);
}

fn watch_failure(
    kind: &'static str,
    summary: &'static str,
    code: &'static str,
) -> CalendarFailure<'static> {
    CalendarFailure {
        domain: FailureDomain::Watch,
        kind,
        summary,
        code,
    }
}
