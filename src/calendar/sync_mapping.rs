use crate::calendar::api::{CalendarAttendee, CalendarEvent, CalendarEventTime};
use crate::calendar::state::CalendarEventSnapshot;
use crate::events::IncomingEvent;
use serde_json::json;

pub(super) fn snapshot(event: &CalendarEvent) -> CalendarEventSnapshot {
    CalendarEventSnapshot {
        revision: event
            .updated
            .clone()
            .or_else(|| event.created.clone())
            .unwrap_or_else(|| event.status.clone()),
        summary: event
            .summary
            .clone()
            .unwrap_or_else(|| "Untitled event".to_string()),
        start: event.start.as_ref().and_then(event_time),
        end: event.end.as_ref().and_then(event_time),
        attendees: event.attendees.iter().map(attendee).collect(),
        meeting_link: event.hangout_link.clone(),
        html_link: event.html_link.clone(),
        created: event.created.clone(),
        updated: event.updated.clone(),
    }
}

fn event_time(value: &CalendarEventTime) -> Option<String> {
    value.date_time.clone().or_else(|| value.date.clone())
}

fn attendee(value: &CalendarAttendee) -> String {
    value
        .display_name
        .as_ref()
        .map(|name| format!("{name} <{}>", value.email))
        .unwrap_or_else(|| value.email.clone())
}

pub(super) fn incoming_event(
    kind: &str,
    event: &CalendarEvent,
    snapshot: CalendarEventSnapshot,
) -> IncomingEvent {
    IncomingEvent {
        kind: kind.to_string(),
        channel: None,
        mention: None,
        format: None,
        template: None,
        payload: json!({
            "source": "google.calendar", "event_id": event.id, "summary": snapshot.summary,
            "start": snapshot.start, "end": snapshot.end, "attendees": snapshot.attendees,
            "meeting_link": snapshot.meeting_link, "html_link": snapshot.html_link,
            "created": snapshot.created, "updated": snapshot.updated,
        }),
    }
}
