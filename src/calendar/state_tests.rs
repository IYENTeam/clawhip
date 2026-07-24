use std::os::unix::fs::{PermissionsExt, symlink};

use serde_json::json;

use super::CalendarState;
use crate::events::IncomingEvent;

#[test]
fn persists_outbox_before_delivery() {
    let temp = tempfile::tempdir().expect("temporary state directory");
    let path = temp.path().join("state.json");
    let mut state = CalendarState::default();
    state.queue(vec![IncomingEvent {
        kind: "calendar.event.created".into(),
        channel: None,
        mention: None,
        format: None,
        template: None,
        payload: json!({"event_id": "event-1"}),
    }]);
    state.save(&path).expect("save queued event");
    assert_eq!(
        CalendarState::load(&path)
            .expect("load queued event")
            .outbox
            .len(),
        1
    );
    assert_eq!(
        std::fs::metadata(path)
            .expect("state metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn queues_a_persisted_opaque_delivery_receipt() {
    let mut state = CalendarState::default();
    state.queue(vec![IncomingEvent {
        kind: "calendar.event.created".into(),
        channel: None,
        mention: None,
        format: None,
        template: None,
        payload: json!({"event_id": "event-1"}),
    }]);

    let receipt = super::delivery_receipt(&state.outbox[0]).expect("delivery receipt");
    assert!(uuid::Uuid::parse_str(receipt).is_ok());
}

#[test]
fn rejects_insecure_and_symlinked_state_files() {
    let temp = tempfile::tempdir().expect("temporary state directory");
    let path = temp.path().join("state.json");
    std::fs::write(&path, "{}").expect("write state fixture");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        .expect("make state fixture public");
    assert!(CalendarState::load(&path).is_err());
    std::fs::remove_file(&path).expect("remove state fixture");
    let target = temp.path().join("target.json");
    std::fs::write(&target, "{}").expect("write symlink target");
    symlink(&target, &path).expect("create state symlink");
    assert!(CalendarState::load(&path).is_err());
}
