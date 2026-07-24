pub mod api;
pub mod credentials;
pub mod journal;
pub mod operations;
pub mod resource;
pub mod state;
pub mod sync;
pub mod watch;

pub(crate) use journal::DeferredCalendarNotification;

use tokio::sync::oneshot;

#[derive(Debug)]
pub struct CalendarNotification {
    pub channel_id: String,
    pub resource_id: String,
    pub resource_uri: String,
    pub message_number: u64,
    pub resource_state: String,
    pub persisted: oneshot::Sender<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalendarDeliveryReceipt {
    pub id: String,
    pub delivered: bool,
}
