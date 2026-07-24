use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::Result;
use crate::calendar::CalendarNotification;
use crate::calendar::state::CalendarState;
use crate::calendar::sync::accept_notification;
use crate::config::AppConfig;

const MAX_DEFERRED_NOTIFICATIONS: usize = 32;
static JOURNAL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct DeferredCalendarNotification {
    pub channel_id: String,
    pub resource_id: String,
    pub resource_uri: String,
    pub message_number: u64,
    pub resource_state: String,
}

pub(crate) fn append_deferred_notification(
    state_path: &Path,
    notification: &DeferredCalendarNotification,
) -> Result<()> {
    with_journal_lock(|| {
        let mut notifications = read_deferred_notifications_locked(state_path)?;
        if notifications
            .iter()
            .any(|existing| same_notification(existing, notification))
        {
            return Ok(());
        }
        notifications.push(notification.clone());
        if notifications.len() > MAX_DEFERRED_NOTIFICATIONS {
            notifications.drain(..notifications.len() - MAX_DEFERRED_NOTIFICATIONS);
        }
        write_deferred_notifications_locked(state_path, &notifications)
    })
}

#[cfg(test)]
pub(crate) fn read_deferred_notifications(
    state_path: &Path,
) -> Result<Vec<DeferredCalendarNotification>> {
    with_journal_lock(|| read_deferred_notifications_locked(state_path))
}

pub(crate) fn remove_deferred_notification(
    state_path: &Path,
    channel_id: &str,
    message_number: u64,
) -> Result<()> {
    with_journal_lock(|| {
        let notifications = read_deferred_notifications_locked(state_path)?;
        let retained = notifications
            .into_iter()
            .filter(|notification| {
                notification.channel_id != channel_id
                    || notification.message_number != message_number
            })
            .collect::<Vec<_>>();
        write_deferred_notifications_locked(state_path, &retained)
    })
}

pub(crate) fn persist_deferred_notification(
    config: &AppConfig,
    notification: DeferredCalendarNotification,
) -> Result<()> {
    let state_path = config
        .google_calendar
        .state_file
        .as_deref()
        .context("Google Calendar state_file is required")?;
    append_deferred_notification(state_path, &notification)
}

pub(crate) fn replay_deferred_notifications(
    state: &mut CalendarState,
    state_path: &Path,
    calendar_id: &str,
) -> Result<()> {
    with_journal_lock(|| {
        let notifications = read_deferred_notifications_locked(state_path)?;
        for deferred in notifications {
            let (persisted, _ack) = oneshot::channel();
            let notification = CalendarNotification {
                channel_id: deferred.channel_id,
                resource_id: deferred.resource_id,
                resource_uri: deferred.resource_uri,
                message_number: deferred.message_number,
                resource_state: deferred.resource_state,
                persisted,
            };
            accept_notification(state, state_path, &notification, calendar_id)?;
        }
        write_deferred_notifications_locked(state_path, &[])
    })
}

fn with_journal_lock<T>(operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let lock = JOURNAL_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
    operation()
}

fn same_notification(
    left: &DeferredCalendarNotification,
    right: &DeferredCalendarNotification,
) -> bool {
    left.channel_id == right.channel_id
        && left.resource_id == right.resource_id
        && left.resource_uri == right.resource_uri
        && left.message_number == right.message_number
        && left.resource_state == right.resource_state
}

fn read_deferred_notifications_locked(
    state_path: &Path,
) -> Result<Vec<DeferredCalendarNotification>> {
    let path = deferred_notification_path(state_path)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if std::fs::symlink_metadata(parent)
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(Vec::new());
    }
    validate_parent(&path)?;
    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
        validate_file(&metadata, &path)?;
    }
    match std::fs::read_to_string(&path) {
        Ok(contents) => contents
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).map_err(Into::into))
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn write_deferred_notifications_locked(
    state_path: &Path,
    notifications: &[DeferredCalendarNotification],
) -> Result<()> {
    let path = deferred_notification_path(state_path)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if notifications.is_empty() {
        if std::fs::symlink_metadata(parent)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            return Ok(());
        }
        validate_parent(&path)?;
        if let Ok(metadata) = std::fs::symlink_metadata(&path) {
            validate_file(&metadata, &path)?;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => File::open(parent)?.sync_all()?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        return Ok(());
    }
    std::fs::create_dir_all(parent)?;
    validate_parent(&path)?;
    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
        validate_file(&metadata, &path)?;
    }
    let temporary = parent.join(format!(".{}.{}.tmp", file_name(&path)?, Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    for notification in notifications {
        file.write_all(&serde_json::to_vec(notification)?)?;
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    std::fs::rename(temporary, &path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn deferred_notification_path(state_path: &Path) -> Result<std::path::PathBuf> {
    let parent = state_path.parent().unwrap_or_else(|| Path::new("."));
    Ok(parent.join(format!(".{}.notifications.jsonl", file_name(state_path)?)))
}

fn file_name(path: &Path) -> Result<&str> {
    path.file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| std::io::Error::other("Calendar state path must name a UTF-8 file").into())
}

fn validate_parent(path: &Path) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let metadata = std::fs::symlink_metadata(parent)
        .with_context(|| format!("inspect Calendar state parent {}", parent.display()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::other("Calendar state parent must be a real directory").into());
    }
    #[cfg(unix)]
    if std::os::unix::fs::MetadataExt::mode(&metadata) & 0o022 != 0 {
        return Err(std::io::Error::other(
            "Calendar state parent must not be group/other-writable",
        )
        .into());
    }
    Ok(())
}

fn validate_file(metadata: &std::fs::Metadata, path: &Path) -> Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::other(format!(
            "Calendar state {} must be a regular file",
            path.display()
        ))
        .into());
    }
    #[cfg(unix)]
    if std::os::unix::fs::MetadataExt::mode(metadata) & 0o077 != 0 {
        return Err(std::io::Error::other(
            "Calendar state must not be accessible by group or other",
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
