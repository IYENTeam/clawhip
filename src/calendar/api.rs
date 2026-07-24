use anyhow::{Context, anyhow};
use reqwest::{Client, Url};
use serde::Deserialize;
use serde::Serialize;
use uuid::Uuid;

use crate::Result;
use crate::calendar::credentials::AuthorizedUserCredentials;
use crate::calendar::state::WatchChannel;

pub struct CalendarApi {
    client: Client,
    credentials: AuthorizedUserCredentials,
    events_url: Url,
    watch_url: Url,
    stop_url: Url,
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod tests;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarEvent {
    pub id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    pub updated: Option<String>,
    #[serde(default)]
    pub start: Option<CalendarEventTime>,
    #[serde(default)]
    pub end: Option<CalendarEventTime>,
    #[serde(default)]
    pub attendees: Vec<CalendarAttendee>,
    #[serde(default)]
    pub hangout_link: Option<String>,
    #[serde(default)]
    pub html_link: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarEventTime {
    #[serde(default)]
    pub date_time: Option<String>,
    #[serde(default)]
    pub date: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarAttendee {
    pub email: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

pub struct SyncBatch {
    pub events: Vec<CalendarEvent>,
    pub next_sync_token: String,
}

#[derive(Serialize)]
struct WatchRequest<'a> {
    id: String,
    #[serde(rename = "type")]
    channel_type: &'static str,
    address: &'a str,
    token: &'a str,
    expiration: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WatchResponse {
    id: String,
    resource_id: String,
    resource_uri: String,
    expiration: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StopRequest<'a> {
    id: &'a str,
    resource_id: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventsPage {
    #[serde(default)]
    items: Vec<CalendarEvent>,
    #[serde(default)]
    next_page_token: Option<String>,
    #[serde(default)]
    next_sync_token: Option<String>,
}

impl CalendarApi {
    pub fn new(
        base_url: &str,
        calendar_id: &str,
        credentials: AuthorizedUserCredentials,
    ) -> Result<Self> {
        let mut events_url = Url::parse(base_url)?;
        events_url
            .path_segments_mut()
            .map_err(|_| anyhow!("Google Calendar API base URL cannot be a base"))?
            .extend(["calendars", calendar_id, "events"]);
        let mut watch_url = events_url.clone();
        watch_url
            .path_segments_mut()
            .map_err(|_| anyhow!("Google Calendar watch URL cannot be a base"))?
            .push("watch");
        let mut stop_url = Url::parse(base_url)?;
        stop_url
            .path_segments_mut()
            .map_err(|_| anyhow!("Google Calendar API base URL cannot be a base"))?
            .extend(["channels", "stop"]);
        Ok(Self {
            client: Client::builder()
                .connect_timeout(connect_timeout())
                .timeout(request_timeout())
                .build()?,
            credentials,
            events_url,
            watch_url,
            stop_url,
        })
    }

    pub async fn create_watch(
        &self,
        callback_url: &str,
        channel_token: &str,
        expiration_ms: i64,
    ) -> Result<WatchChannel> {
        let access_token = self.credentials.access_token(&self.client).await?;
        let response = self
            .client
            .post(self.watch_url.clone())
            .bearer_auth(access_token)
            .json(&WatchRequest {
                id: Uuid::new_v4().to_string(),
                channel_type: "web_hook",
                address: callback_url,
                token: channel_token,
                expiration: expiration_ms.to_string(),
            })
            .send()
            .await?
            .error_for_status()?
            .json::<WatchResponse>()
            .await?;
        Ok(WatchChannel {
            id: response.id,
            resource_id: response.resource_id,
            resource_uri: response.resource_uri,
            expiration_ms: response.expiration.parse()?,
            activation_deadline_ms: None,
        })
    }

    pub async fn stop_watch(&self, channel: &WatchChannel) -> Result<()> {
        let access_token = self.credentials.access_token(&self.client).await?;
        let response = self
            .client
            .post(self.stop_url.clone())
            .bearer_auth(access_token)
            .json(&StopRequest {
                id: &channel.id,
                resource_id: &channel.resource_id,
            })
            .send()
            .await?;
        if matches!(
            response.status(),
            reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::GONE
        ) {
            return Ok(());
        }
        response.error_for_status()?;
        Ok(())
    }

    pub async fn establish_initial_sync(&self) -> Result<SyncBatch> {
        self.fetch_sync(None)
            .await?
            .ok_or_else(|| anyhow!("initial Calendar sync unexpectedly returned HTTP 410").into())
    }

    pub async fn incremental_sync(&self, sync_token: &str) -> Result<Option<SyncBatch>> {
        self.fetch_sync(Some(sync_token)).await
    }

    async fn fetch_sync(&self, sync_token: Option<&str>) -> Result<Option<SyncBatch>> {
        let access_token = self.credentials.access_token(&self.client).await?;
        let mut page_token: Option<String> = None;
        let mut events = Vec::new();
        loop {
            let mut request = self
                .client
                .get(self.events_url.clone())
                .bearer_auth(&access_token)
                .query(&[("showDeleted", "true"), ("maxResults", "2500")]);
            if let Some(token) = sync_token {
                request = request.query(&[("syncToken", token)]);
            }
            if let Some(token) = page_token.as_deref() {
                request = request.query(&[("pageToken", token)]);
            }
            let response = request.send().await?;
            if sync_token.is_some() && response.status() == reqwest::StatusCode::GONE {
                return Ok(None);
            }
            let page = response
                .error_for_status()?
                .json::<EventsPage>()
                .await
                .context("decode Google Calendar events page")?;
            events.extend(page.items);
            if let Some(token) = page.next_page_token {
                page_token = Some(token);
                continue;
            }
            let next_sync_token = page.next_sync_token.ok_or_else(|| {
                anyhow!("final Google Calendar events page omitted nextSyncToken")
            })?;
            return Ok(Some(SyncBatch {
                events,
                next_sync_token,
            }));
        }
    }
}

#[cfg(not(test))]
fn request_timeout() -> Duration {
    Duration::from_secs(30)
}

#[cfg(test)]
fn request_timeout() -> Duration {
    Duration::from_millis(100)
}

fn connect_timeout() -> Duration {
    Duration::from_secs(10)
}
use std::time::Duration;
