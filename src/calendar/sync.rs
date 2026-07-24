use crate::Result;
use crate::calendar::CalendarNotification;
use crate::calendar::api::{CalendarApi, CalendarEvent};
use crate::calendar::operations::{
    CalendarFailure, FailureDomain, WATCH_ACTIVATION_TIMED_OUT, retire_active_watch,
};
use crate::calendar::resource;
use crate::calendar::state::CalendarState;
use crate::events::IncomingEvent;
use crate::source::now_rfc3339;
use anyhow::Context;

#[path = "sync_mapping.rs"]
mod mapping;

pub fn replace_baseline(state: &mut CalendarState, events: &[CalendarEvent]) {
    state.events = events
        .iter()
        .filter(|event| event.status != "cancelled")
        .map(|event| (event.id.clone(), mapping::snapshot(event)))
        .collect();
}

pub fn apply_incremental(
    state: &mut CalendarState,
    events: &[CalendarEvent],
) -> Vec<IncomingEvent> {
    let mut emitted = Vec::new();
    for event in events {
        if event.status == "cancelled" {
            let prior = state.events.remove(&event.id);
            emitted.push(mapping::incoming_event(
                "calendar.event.cancelled",
                event,
                prior.unwrap_or_else(|| mapping::snapshot(event)),
            ));
            continue;
        }
        let next = mapping::snapshot(event);
        let kind = match state.events.get(&event.id) {
            None => "calendar.event.created",
            Some(previous) if previous.revision != next.revision => "calendar.event.updated",
            Some(_) => continue,
        };
        state.events.insert(event.id.clone(), next.clone());
        emitted.push(mapping::incoming_event(kind, event, next));
    }
    emitted
}

#[derive(Clone)]
pub enum SyncJob {
    Initial,
    Incremental { channel: String, message: u64 },
    Recovery { channel: String, message: u64 },
}

pub enum SyncResult {
    Done,
    Recover(SyncJob),
}

/// The outcome of authenticating a Calendar callback against persisted watch state.
///
/// Only `Accepted` callbacks are safe to expose as trusted webhook events.
pub enum NotificationAcceptance {
    Accepted(Option<SyncJob>),
    Rejected(NotificationRejection),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationRejection {
    UnsupportedResourceState,
    UntrackedWatch,
    StaleOrReplayed,
}

pub fn accept_notification(
    state: &mut CalendarState,
    path: &std::path::Path,
    notification: &CalendarNotification,
    calendar_id: &str,
) -> Result<NotificationAcceptance> {
    if !matches!(notification.resource_state.as_str(), "sync" | "exists") {
        return Ok(NotificationAcceptance::Rejected(
            NotificationRejection::UnsupportedResourceState,
        ));
    }
    if !matches_watch(state, notification, calendar_id) {
        return Ok(NotificationAcceptance::Rejected(
            NotificationRejection::UntrackedWatch,
        ));
    }
    if !state.is_new_message(&notification.channel_id, notification.message_number)
        || state.pending_sync.as_ref().is_some_and(|pending| {
            pending.channel == notification.channel_id
                && pending.message >= notification.message_number
        })
    {
        return Ok(NotificationAcceptance::Rejected(
            NotificationRejection::StaleOrReplayed,
        ));
    }

    state.last_notification_at = Some(now_rfc3339());
    state.last_message_number = Some(notification.message_number);
    if notification.resource_state == "sync"
        && state
            .pending_watch
            .as_ref()
            .is_some_and(|watch| watch.id == notification.channel_id)
    {
        retire_active_watch(state);
        state.active_watch = state.pending_watch.take();
        state.clear_watch_failure(WATCH_ACTIVATION_TIMED_OUT);
    }
    state.pending_sync = Some(crate::calendar::state::PendingSyncTrigger {
        channel: notification.channel_id.clone(),
        message: notification.message_number,
    });
    state.save(path)?;
    Ok(NotificationAcceptance::Accepted(Some(
        SyncJob::Incremental {
            channel: notification.channel_id.clone(),
            message: notification.message_number,
        },
    )))
}

pub async fn run_sync(
    api: &CalendarApi,
    state: &mut CalendarState,
    path: &std::path::Path,
    job: SyncJob,
) -> Result<SyncResult> {
    match job {
        SyncJob::Initial => {
            let baseline = api.establish_initial_sync().await?;
            replace_baseline(state, &baseline.events);
            state.next_sync_token = Some(baseline.next_sync_token);
        }
        SyncJob::Incremental { channel, message } => {
            let token = state
                .next_sync_token
                .as_deref()
                .context("Calendar sync token is missing")?;
            let Some(batch) = api.incremental_sync(token).await? else {
                state.next_sync_token = None;
                state.save(path)?;
                return Ok(SyncResult::Recover(SyncJob::Recovery { channel, message }));
            };
            let events = apply_incremental(state, &batch.events);
            state.queue(events);
            state.next_sync_token = Some(batch.next_sync_token);
            complete_pending_sync(state, &channel, message);
        }
        SyncJob::Recovery { channel, message } => {
            let baseline = api.establish_initial_sync().await?;
            replace_baseline(state, &baseline.events);
            state.next_sync_token = Some(baseline.next_sync_token);
            complete_pending_sync(state, &channel, message);
        }
    }
    state.last_successful_sync_at = Some(now_rfc3339());
    state.sync_error = None;
    state.save(path)?;
    Ok(SyncResult::Done)
}

pub fn complete_pending_sync(state: &mut CalendarState, channel: &str, message: u64) {
    state.record_message(channel, message);
    if state
        .pending_sync
        .as_ref()
        .is_some_and(|pending| pending.channel == channel && pending.message == message)
    {
        state.pending_sync = None;
    }
}

pub fn remove_outbox_receipt(
    state: &mut CalendarState,
    path: &std::path::Path,
    receipt_id: &str,
) -> Result<bool> {
    let Some(event) = state.outbox.first() else {
        return Ok(false);
    };
    if crate::calendar::state::delivery_receipt(event) != Some(receipt_id) {
        return Ok(false);
    }
    state.outbox.remove(0);
    state.save(path)?;
    Ok(true)
}

pub fn sync_failure(job: &SyncJob) -> CalendarFailure<'static> {
    let (kind, summary, code) = match job {
        SyncJob::Initial => (
            "calendar.sync.initial-failed",
            "Calendar initial sync failed",
            "calendar_initial_sync_failed",
        ),
        SyncJob::Incremental { .. } => (
            "calendar.sync.failed",
            "Calendar incremental sync failed",
            "calendar_incremental_sync_failed",
        ),
        SyncJob::Recovery { .. } => (
            "calendar.sync.recovery-failed",
            "Calendar sync recovery failed",
            "calendar_sync_recovery_failed",
        ),
    };
    CalendarFailure {
        domain: FailureDomain::Sync,
        kind,
        summary,
        code,
    }
}

fn matches_watch(
    state: &CalendarState,
    notification: &CalendarNotification,
    calendar_id: &str,
) -> bool {
    state
        .active_watch
        .iter()
        .chain(state.pending_watch.iter())
        .any(|watch| {
            watch.id == notification.channel_id
                && watch.resource_id == notification.resource_id
                && resource::equivalent(
                    &watch.resource_uri,
                    &notification.resource_uri,
                    calendar_id,
                )
        })
}

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
