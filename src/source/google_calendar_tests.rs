use crate::calendar::journal::{
    DeferredCalendarNotification, append_deferred_notification, read_deferred_notifications,
    replay_deferred_notifications,
};
use crate::calendar::state::{CalendarState, WatchChannel};

#[test]
fn replayed_journal_survives_a_stale_state_save_and_restores_pending_sync() {
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
    state.save(&path).expect("save initial state");

    let mut stale = CalendarState::load(&path).expect("load stale state");
    append_deferred_notification(
        &path,
        &DeferredCalendarNotification {
            channel_id: "channel-1".into(),
            resource_id: "resource-1".into(),
            resource_uri: "https://www.googleapis.com/calendar/v3/calendars/primary/events".into(),
            message_number: 2,
            resource_state: "exists".into(),
        },
    )
    .expect("append durable callback journal entry");
    stale.watch_error = Some("stale writer".into());
    stale.save(&path).expect("stale state save");

    let mut restarted = CalendarState::load(&path).expect("reload state");
    replay_deferred_notifications(&mut restarted, &path, "primary")
        .expect("replay durable callback journal");

    assert_eq!(
        restarted
            .pending_sync
            .as_ref()
            .map(|pending| pending.message),
        Some(2)
    );
    assert!(
        read_deferred_notifications(&path)
            .expect("read cleared journal")
            .is_empty()
    );
}
