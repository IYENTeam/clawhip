use std::sync::{Arc, Barrier};

use super::{
    DeferredCalendarNotification, append_deferred_notification, read_deferred_notifications,
    remove_deferred_notification,
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
            remove_deferred_notification(&remove_path, "channel-1", 1)
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
fn journal_deduplicates_and_retains_only_the_latest_32_callbacks() {
    let temp = tempfile::tempdir().expect("temporary journal directory");
    let state_path = temp.path().join("calendar-state.json");
    for message_number in 0..40 {
        append_deferred_notification(&state_path, &notification(message_number))
            .expect("append callback");
    }
    append_deferred_notification(&state_path, &notification(39)).expect("deduplicate callback");

    let notifications = read_deferred_notifications(&state_path).expect("read bounded journal");
    assert_eq!(notifications.len(), 32);
    assert_eq!(
        notifications
            .first()
            .map(|notification| notification.message_number),
        Some(8)
    );
    assert_eq!(
        notifications
            .last()
            .map(|notification| notification.message_number),
        Some(39)
    );
}

#[test]
fn journal_deduplicates_only_identical_callbacks() {
    let temp = tempfile::tempdir().expect("temporary journal directory");
    let state_path = temp.path().join("calendar-state.json");
    let first = notification(7);
    let mut conflicting = notification(7);
    conflicting.resource_id = "resource-2".into();

    append_deferred_notification(&state_path, &first).expect("append first callback");
    append_deferred_notification(&state_path, &conflicting).expect("append non-identical callback");

    let notifications = read_deferred_notifications(&state_path).expect("read journal");
    assert_eq!(notifications.len(), 2);
    assert_eq!(
        notifications
            .iter()
            .map(|notification| notification.resource_id.as_str())
            .collect::<Vec<_>>(),
        vec!["resource-1", "resource-2"]
    );
}
