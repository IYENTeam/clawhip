use super::*;
use crate::calendar::state::WatchChannel;
use tokio::sync::oneshot;

#[test]
fn accepted_notification_persists_pending_trigger_until_sync_completes() {
    let temp = tempfile::tempdir().expect("temporary state directory");
    let path = temp.path().join("calendar-state.json");
    let mut state = CalendarState::default();
    state.next_sync_token = Some("cursor".into());
    state.active_watch = Some(WatchChannel {
        id: "channel-1".into(),
        resource_id: "resource-1".into(),
        resource_uri: "https://www.googleapis.com/calendar/v3/calendars/primary/events".into(),
        expiration_ms: 1,
        activation_deadline_ms: None,
    });
    let (persisted, _ack) = oneshot::channel();
    let notification = CalendarNotification {
        channel_id: "channel-1".into(),
        resource_id: "resource-1".into(),
        resource_uri: "https://www.googleapis.com/calendar/v3/calendars/primary/events".into(),
        message_number: 2,
        resource_state: "exists".into(),
        persisted,
    };

    accept_notification(&mut state, &path, &notification, "primary").expect("persist trigger");
    assert_eq!(
        CalendarState::load(&path)
            .expect("load persisted trigger")
            .pending_sync
            .expect("pending trigger")
            .message,
        2
    );

    complete_pending_sync(&mut state, "channel-1", 2);
    assert!(state.pending_sync.is_none());
    assert_eq!(state.message_numbers.get("channel-1"), Some(&2));
}
