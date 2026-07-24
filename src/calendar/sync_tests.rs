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

    assert!(matches!(
        accept_notification(&mut state, &path, &notification, "primary").expect("persist trigger"),
        NotificationAcceptance::Accepted(Some(SyncJob::Incremental { .. }))
    ));
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

#[test]
fn pending_watch_sync_activation_clears_its_timeout_failure() {
    let temp = tempfile::tempdir().expect("temporary state directory");
    let path = temp.path().join("calendar-state.json");
    let mut state = CalendarState::default();
    state.pending_watch = Some(WatchChannel {
        id: "pending-channel".into(),
        resource_id: "pending-resource".into(),
        resource_uri: "https://www.googleapis.com/calendar/v3/calendars/primary/events".into(),
        expiration_ms: 1,
        activation_deadline_ms: Some(1),
    });
    state.record_watch_failure("calendar_watch_activation_timed_out");
    let (persisted, _ack) = oneshot::channel();
    let notification = CalendarNotification {
        channel_id: "pending-channel".into(),
        resource_id: "pending-resource".into(),
        resource_uri: "https://www.googleapis.com/calendar/v3/calendars/primary/events".into(),
        message_number: 1,
        resource_state: "sync".into(),
        persisted,
    };

    assert!(matches!(
        accept_notification(&mut state, &path, &notification, "primary")
            .expect("activate pending watch"),
        NotificationAcceptance::Accepted(Some(SyncJob::Incremental { .. }))
    ));
    assert_eq!(
        state.active_watch.as_ref().map(|watch| watch.id.as_str()),
        Some("pending-channel")
    );
    assert!(!state.has_watch_failures());
    assert!(state.watch_error.is_none());
}

#[test]
fn untracked_and_replayed_notifications_are_rejected_before_state_mutation() {
    let temp = tempfile::tempdir().expect("temporary state directory");
    let path = temp.path().join("calendar-state.json");
    let mut state = CalendarState::default();
    state.active_watch = Some(WatchChannel {
        id: "channel-1".into(),
        resource_id: "resource-1".into(),
        resource_uri: "https://www.googleapis.com/calendar/v3/calendars/primary/events".into(),
        expiration_ms: 1,
        activation_deadline_ms: None,
    });
    state.record_message("channel-1", 42);

    let (persisted, _ack) = oneshot::channel();
    let untracked = CalendarNotification {
        channel_id: "untracked".into(),
        resource_id: "resource-1".into(),
        resource_uri: "https://www.googleapis.com/calendar/v3/calendars/primary/events".into(),
        message_number: 43,
        resource_state: "exists".into(),
        persisted,
    };
    assert!(matches!(
        accept_notification(&mut state, &path, &untracked, "primary").expect("validate watch"),
        NotificationAcceptance::Rejected(NotificationRejection::UntrackedWatch)
    ));

    let (persisted, _ack) = oneshot::channel();
    let replayed = CalendarNotification {
        channel_id: "channel-1".into(),
        resource_id: "resource-1".into(),
        resource_uri: "https://www.googleapis.com/calendar/v3/calendars/primary/events".into(),
        message_number: 42,
        resource_state: "exists".into(),
        persisted,
    };
    assert!(matches!(
        accept_notification(&mut state, &path, &replayed, "primary").expect("validate replay"),
        NotificationAcceptance::Rejected(NotificationRejection::StaleOrReplayed)
    ));
    assert_eq!(state.message_numbers.get("channel-1"), Some(&42));
    assert!(state.pending_sync.is_none());
}
