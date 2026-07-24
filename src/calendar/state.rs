use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Result;
use crate::events::IncomingEvent;

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct CalendarState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_sync_token: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub events: BTreeMap<String, CalendarEventSnapshot>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub message_numbers: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    channel_order: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_watch: Option<WatchChannel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_watch: Option<WatchChannel>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retiring_watches: Vec<WatchChannel>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outbox: Vec<IncomingEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_sync: Option<PendingSyncTrigger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_notification_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_successful_sync_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message_number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PendingSyncTrigger {
    pub channel: String,
    pub message: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WatchChannel {
    pub id: String,
    pub resource_id: String,
    pub resource_uri: String,
    pub expiration_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_deadline_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CalendarEventSnapshot {
    pub revision: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attendees: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meeting_link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html_link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
}

impl CalendarState {
    pub fn is_new_message(&self, channel_id: &str, message_number: u64) -> bool {
        self.message_numbers
            .get(channel_id)
            .is_none_or(|previous| message_number > *previous)
    }

    pub fn record_message(&mut self, channel_id: &str, message_number: u64) {
        const MAX_CHANNELS: usize = 32;
        self.message_numbers
            .insert(channel_id.to_string(), message_number);
        self.channel_order.retain(|channel| channel != channel_id);
        self.channel_order.push(channel_id.to_string());
        while self.channel_order.len() > MAX_CHANNELS {
            if let Some(expired) = self.channel_order.first().cloned() {
                self.channel_order.remove(0);
                self.message_numbers.remove(&expired);
            }
        }
    }

    pub fn queue(&mut self, events: Vec<IncomingEvent>) {
        self.outbox
            .extend(events.into_iter().map(with_delivery_receipt));
    }

    pub fn ensure_outbox_receipts(&mut self) -> bool {
        let mut changed = false;
        for event in &mut self.outbox {
            if delivery_receipt(event).is_none() {
                *event = with_delivery_receipt(event.clone());
                changed = true;
            }
        }
        changed
    }

    pub fn load(path: &Path) -> Result<Self> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        if std::fs::symlink_metadata(parent)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            return Ok(Self::default());
        }
        validate_parent(path)?;
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => {
                validate_file(&metadata, path)?;
                let bytes = std::fs::read(path)?;
                Ok(serde_json::from_slice(&bytes)
                    .with_context(|| format!("parse Calendar state {}", path.display()))?)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        validate_parent(path)?;
        if let Ok(metadata) = std::fs::symlink_metadata(path) {
            validate_file(&metadata, path)?;
        }
        let temporary = parent.join(format!(".{}.{}.tmp", file_name(path)?, Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    }
}

pub(crate) const DELIVERY_RECEIPT_FIELD: &str = "_calendar_delivery_receipt";

pub(crate) fn delivery_receipt(event: &IncomingEvent) -> Option<&str> {
    event.payload.get(DELIVERY_RECEIPT_FIELD)?.as_str()
}

fn with_delivery_receipt(mut event: IncomingEvent) -> IncomingEvent {
    if let Some(payload) = event.payload.as_object_mut() {
        payload
            .entry(DELIVERY_RECEIPT_FIELD.to_string())
            .or_insert_with(|| serde_json::json!(Uuid::new_v4().to_string()));
    }
    event
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
#[path = "state_tests.rs"]
mod tests;
