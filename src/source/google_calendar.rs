use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::sync::{Mutex, mpsc};

use crate::Result;
use crate::calendar::api::CalendarApi;
use crate::calendar::credentials::AuthorizedUserCredentials;
use crate::calendar::journal::{remove_deferred_notification, replay_deferred_notifications};
use crate::calendar::operations::{
    publish_status, record_failure, refresh_health, retry_delay, watch_cycle,
};
use crate::calendar::state::{CalendarState, delivery_receipt};
use crate::calendar::sync::{
    NotificationAcceptance, SyncJob, SyncResult, accept_notification, remove_outbox_receipt,
    run_sync, sync_failure,
};
use crate::calendar::{CalendarDeliveryReceipt, CalendarNotification};
use crate::config::AppConfig;
use crate::events::IncomingEvent;
use crate::source::{SharedSourceHealth, Source, mark_source_error};

pub struct GoogleCalendarSource {
    config: Arc<AppConfig>,
    health: SharedSourceHealth,
    notifications: Mutex<Option<mpsc::Receiver<CalendarNotification>>>,
    delivery_receipts: Mutex<Option<mpsc::Receiver<CalendarDeliveryReceipt>>>,
}

impl GoogleCalendarSource {
    pub fn new(
        config: Arc<AppConfig>,
        health: SharedSourceHealth,
        notifications: mpsc::Receiver<CalendarNotification>,
        delivery_receipts: mpsc::Receiver<CalendarDeliveryReceipt>,
    ) -> Self {
        Self {
            config,
            health,
            notifications: Mutex::new(Some(notifications)),
            delivery_receipts: Mutex::new(Some(delivery_receipts)),
        }
    }
}

#[async_trait::async_trait]
impl Source for GoogleCalendarSource {
    fn name(&self) -> &str {
        "google-calendar"
    }

    async fn run(&self, tx: mpsc::Sender<IncomingEvent>) -> Result<()> {
        let calendar = &self.config.google_calendar;
        if !calendar.sync_enabled() {
            return Ok(());
        }
        let credentials_path = calendar
            .credentials_file
            .as_deref()
            .context("Google Calendar credentials_file is required")?;
        let state_path = calendar
            .state_file
            .as_deref()
            .context("Google Calendar state_file is required")?;
        let mut notifications = self
            .notifications
            .lock()
            .await
            .take()
            .context("Google Calendar source can only run once")?;
        let mut delivery_receipts = self
            .delivery_receipts
            .lock()
            .await
            .take()
            .context("Google Calendar source can only run once")?;
        let mut bootstrap_attempt: u32 = 0;
        let (api, mut state) = loop {
            let setup = (|| -> Result<(CalendarApi, CalendarState)> {
                let credentials = AuthorizedUserCredentials::load(credentials_path)?;
                let api =
                    CalendarApi::new(&calendar.api_base_url, &calendar.calendar_id, credentials)?;
                let state = CalendarState::load(state_path)?;
                Ok((api, state))
            })();
            match setup {
                Ok(setup) => break setup,
                Err(_error) => {
                    const CODE: &str = "calendar_bootstrap_failed";
                    mark_source_error(&self.health, self.name(), CODE).await;
                    if tx
                        .send(IncomingEvent {
                            kind: "calendar.sync.bootstrap-failed".to_string(),
                            channel: None,
                            mention: None,
                            format: None,
                            template: None,
                            payload: serde_json::json!({
                                "summary": "Calendar synchronization bootstrap failed",
                                "error": CODE,
                            }),
                        })
                        .await
                        .is_err()
                    {
                        return Ok(());
                    }
                    bootstrap_attempt = bootstrap_attempt.saturating_add(1);
                    tokio::time::sleep(retry_delay(bootstrap_attempt)).await;
                }
            }
        };
        if state.ensure_outbox_receipts() {
            state.save(state_path)?;
        }
        replay_deferred_notifications(&mut state, state_path, &calendar.calendar_id)?;
        let mut job = resume_sync_job(&state);
        let mut in_flight_delivery = None;
        let mut attempts: u32 = 0;
        let mut retry_wait = Duration::ZERO;
        let mut delivery_attempts: u32 = 0;
        let mut delivery_retry_wait = Duration::ZERO;
        let mut watch_wait = if job.is_some() {
            Duration::from_secs(86_400)
        } else {
            Duration::ZERO
        };
        let mut watch_attempts: u32 = 0;
        loop {
            if in_flight_delivery.is_none()
                && delivery_retry_wait.is_zero()
                && let Some(event) = state.outbox.first().cloned()
            {
                let receipt = delivery_receipt(&event)
                    .context("Calendar outbox event is missing a delivery receipt")?
                    .to_string();
                tx.send(event)
                    .await
                    .map_err(|error| format!("Calendar dispatcher queue unavailable: {error}"))?;
                in_flight_delivery = Some(receipt);
            }
            let sync_wait =
                if job.is_some() || (!state.outbox.is_empty() && in_flight_delivery.is_none()) {
                    retry_wait
                } else {
                    Duration::from_secs(86_400)
                };
            let renewal_wait =
                if calendar.callback_url.is_some() && calendar.channel_token.is_some() {
                    watch_wait
                } else {
                    Duration::from_secs(86_400)
                };
            tokio::select! {
                notification = notifications.recv() => {
                    let Some(notification) = notification else { return Ok(()); };
                    match accept_notification(
                        &mut state,
                        state_path,
                        &notification,
                        &calendar.calendar_id,
                    ) {
                        Ok(NotificationAcceptance::Accepted(next)) => {
                            remove_deferred_notification(
                                state_path,
                                &notification.channel_id,
                                &notification.resource_id,
                                &notification.resource_uri,
                                notification.message_number,
                            )?;
                            let _ = notification.persisted.send(true);
                            if let Some(next) = next
                                && job.is_none()
                            {
                                job = Some(next);
                                attempts = 0;
                                retry_wait = Duration::ZERO;
                            }
                            watch_wait = Duration::ZERO;
                        }
                        Ok(NotificationAcceptance::Rejected(rejection)) => {
                            eprintln!(
                                "op_pi Google Calendar callback ignored: {rejection:?}"
                            );
                            remove_deferred_notification(
                                state_path,
                                &notification.channel_id,
                                &notification.resource_id,
                                &notification.resource_uri,
                                notification.message_number,
                            )?;
                            let _ = notification.persisted.send(false);
                        }
                        Err(error) => return Err(error),
                    }
                }
                receipt = delivery_receipts.recv(), if in_flight_delivery.is_some() => {
                    let Some(receipt) = receipt else { return Ok(()); };
                    if in_flight_delivery.as_deref() == Some(receipt.id.as_str()) {
                        in_flight_delivery = None;
                        if receipt.delivered {
                            remove_outbox_receipt(&mut state, state_path, &receipt.id)?;
                            delivery_attempts = 0;
                            delivery_retry_wait = Duration::ZERO;
                        } else {
                            delivery_attempts = delivery_attempts.saturating_add(1);
                            delivery_retry_wait = retry_delay(delivery_attempts);
                        }
                    }
                }
                _ = tokio::time::sleep(delivery_retry_wait),
                    if !state.outbox.is_empty()
                        && in_flight_delivery.is_none()
                        && !delivery_retry_wait.is_zero() =>
                {
                    delivery_retry_wait = Duration::ZERO;
                }
                _ = tokio::time::sleep(sync_wait), if job.is_some() => {
                    let Some(active) = job.clone() else { continue; };
                    match run_sync(&api, &mut state, state_path, active).await {
                        Ok(SyncResult::Done) => { job = resume_sync_job(&state); attempts = 0; watch_wait = Duration::ZERO; refresh_health(&self.health, self.name(), &state).await; }
                        Ok(SyncResult::Recover(next)) => { job = Some(next); attempts = 0; retry_wait = Duration::ZERO; }
                        Err(_) => {
                            if let Some(active) = job.as_ref() { record_failure(&self.health, self.name(), &mut state, state_path, sync_failure(active)).await?; }
                            attempts = attempts.saturating_add(1); retry_wait = retry_delay(attempts);
                        }
                    }
                }
                _ = tokio::time::sleep(renewal_wait), if calendar.callback_url.is_some() && calendar.channel_token.is_some() => {
                    let successful_wait = watch_cycle(&api, calendar, &mut state, state_path, &self.health, self.name()).await?;
                    watch_wait = next_watch_wait(&mut watch_attempts, &state, successful_wait);
                }
            }
            publish_status(&self.health, self.name(), &state).await;
        }
    }
}

fn next_watch_wait(
    attempts: &mut u32,
    state: &CalendarState,
    successful_wait: Duration,
) -> Duration {
    if state.has_watch_failures() {
        *attempts = attempts.saturating_add(1);
        retry_delay(*attempts)
    } else {
        *attempts = 0;
        successful_wait
    }
}

fn resume_sync_job(state: &CalendarState) -> Option<SyncJob> {
    if state.next_sync_token.is_none() {
        return Some(SyncJob::Initial);
    }
    if let Some(pending) = state.pending_sync.as_ref() {
        return Some(SyncJob::Incremental {
            channel: pending.channel.clone(),
            message: pending.message,
        });
    }
    let channel = state.active_watch.as_ref()?.id.clone();
    let message = state.last_message_number?;
    state
        .sync_error
        .is_some()
        .then_some(SyncJob::Incremental { channel, message })
}

#[cfg(test)]
#[path = "google_calendar_tests.rs"]
mod tests;
