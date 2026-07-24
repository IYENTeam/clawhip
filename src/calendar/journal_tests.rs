use std::sync::{Arc, Barrier};

use crate::calendar::state::{CalendarState, WatchChannel};

use super::{
    DeferredCalendarNotification, MAX_DEFERRED_NOTIFICATIONS, append_deferred_notification,
    read_deferred_notifications, remove_deferred_notification, replay_deferred_notifications,
};

fn notification(message_number: u64) -> DeferredCalendarNotification {
    DeferredCalendarNotification {
        channel_id: "channel-1".into(),
        resource_id: "resource-1".into(),
        resource_uri: "https://www.googleapis.com/calendar/v3/calendars/primary/events".into(),
        message_number,
        resource_state: "exists".into(),
    }
}

#[test]
fn concurrent_append_and_remove_preserve_the_new_callback() {
    let temp = tempfile::tempdir().expect("temporary journal directory");
    let state_path = temp.path().join("calendar-state.json");
    append_deferred_notification(&state_path, &notification(1)).expect("seed journal");

    let barrier = Arc::new(Barrier::new(3));
    std::thread::scope(|scope| {
        let append_barrier = barrier.clone();
        let append_path = state_path.clone();
        scope.spawn(move || {
            append_barrier.wait();
            append_deferred_notification(&append_path, &notification(2)).expect("append callback");
        });
        let remove_barrier = barrier.clone();
        let remove_path = state_path.clone();
        scope.spawn(move || {
            remove_barrier.wait();
            remove_deferred_notification(
                &remove_path,
                "channel-1",
                "resource-1",
                "https://www.googleapis.com/calendar/v3/calendars/primary/events",
                1,
            )
            .expect("remove processed callback");
        });
        barrier.wait();
    });

    let notifications = read_deferred_notifications(&state_path).expect("read journal");
    assert_eq!(
        notifications
            .iter()
            .map(|notification| notification.message_number)
            .collect::<Vec<_>>(),
        vec![2]
    );
}

#[test]
fn journal_retains_the_highest_message_number_per_identity_at_u64_boundaries() {
    let temp = tempfile::tempdir().expect("temporary journal directory");
    let state_path = temp.path().join("calendar-state.json");
    for message_number in [0, u64::MAX - 1, u64::MAX, 0] {
        append_deferred_notification(&state_path, &notification(message_number))
            .expect("append callback");
    }

    let notifications = read_deferred_notifications(&state_path).expect("read journal");
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].message_number, u64::MAX);
}

#[test]
fn journal_rejects_new_identity_when_global_bound_is_full() {
    let temp = tempfile::tempdir().expect("temporary journal directory");
    let state_path = temp.path().join("calendar-state.json");
    for index in 0..MAX_DEFERRED_NOTIFICATIONS {
        let mut callback = notification(index as u64);
        callback.channel_id = format!("callback-{index:02}");
        append_deferred_notification(&state_path, &callback).expect("append callback");
    }

    let mut activation = notification(1);
    activation.channel_id = "activation".into();
    activation.resource_state = "sync".into();
    let error = append_deferred_notification(&state_path, &activation)
        .expect_err("full journal must reject a new identity");
    assert!(error.to_string().contains("journal is full"));

    let notifications = read_deferred_notifications(&state_path).expect("read bounded journal");
    assert_eq!(notifications.len(), MAX_DEFERRED_NOTIFICATIONS);
    assert!(!notifications.contains(&activation));
}

#[test]
fn journal_preserves_sync_and_exists_at_equal_numbers_for_the_same_identity() {
    let temp = tempfile::tempdir().expect("temporary journal directory");
    let state_path = temp.path().join("calendar-state.json");
    let callback = notification(7);
    let mut activation = notification(7);
    activation.resource_state = "sync".into();

    append_deferred_notification(&state_path, &callback).expect("append callback");
    append_deferred_notification(&state_path, &activation).expect("append activation");

    let notifications = read_deferred_notifications(&state_path).expect("read journal");
    assert_eq!(notifications, vec![activation, callback]);
}

#[test]
fn later_exists_callback_cannot_displace_pending_sync_activation() {
    let temp = tempfile::tempdir().expect("temporary journal directory");
    let state_path = temp.path().join("calendar-state.json");
    let mut activation = notification(7);
    activation.resource_state = "sync".into();
    let callback = notification(8);

    append_deferred_notification(&state_path, &activation).expect("append activation");
    append_deferred_notification(&state_path, &callback).expect("append callback");

    let notifications = read_deferred_notifications(&state_path).expect("read journal");
    assert_eq!(notifications, vec![activation, callback]);
}

#[test]
fn forged_identity_cannot_replace_or_block_a_valid_deferred_callback() {
    let temp = tempfile::tempdir().expect("temporary state directory");
    let state_path = temp.path().join("calendar-state.json");
    let mut state = CalendarState::default();
    state.next_sync_token = Some("cursor".into());
    state.active_watch = Some(WatchChannel {
        id: "channel-1".into(),
        resource_id: "resource-1".into(),
        resource_uri: "https://www.googleapis.com/calendar/v3/calendars/primary/events".into(),
        expiration_ms: 1,
        activation_deadline_ms: None,
    });
    state.save(&state_path).expect("save active watch");

    let valid = notification(2);
    let mut forged = notification(u64::MAX);
    forged.resource_id = "forged-resource".into();
    forged.resource_uri = "https://attacker.invalid/events".into();
    append_deferred_notification(&state_path, &forged).expect("append forged callback");
    append_deferred_notification(&state_path, &valid).expect("append valid callback");

    let notifications = read_deferred_notifications(&state_path).expect("read journal");
    assert_eq!(notifications.len(), 2);
    assert!(notifications.contains(&valid));
    assert!(notifications.contains(&forged));

    replay_deferred_notifications(&mut state, &state_path, "primary").expect("replay journal");

    assert_eq!(
        state.pending_sync.as_ref().map(|pending| pending.message),
        Some(valid.message_number)
    );
    assert!(
        read_deferred_notifications(&state_path)
            .expect("read cleared journal")
            .is_empty()
    );
}
