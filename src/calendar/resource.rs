use reqwest::Url;

pub fn equivalent(expected: &str, received: &str, requested_calendar_id: &str) -> bool {
    if expected == received {
        return true;
    }
    let (Some(expected_id), Some(received_id)) = (calendar_id(expected), calendar_id(received))
    else {
        return false;
    };
    expected_id == received_id
        || (requested_calendar_id == "primary"
            && (expected_id == "primary" || received_id == "primary"))
}

fn calendar_id(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("www.googleapis.com")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let segments = url.path_segments()?.collect::<Vec<_>>();
    match segments.as_slice() {
        ["calendar", "v3", "calendars", calendar_id, "events"] => Some((*calendar_id).to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::equivalent;

    #[test]
    fn accepts_primary_alias_for_same_google_calendar_collection() {
        assert!(equivalent(
            "https://www.googleapis.com/calendar/v3/calendars/victor%40arkpoint.kr/events?alt=json",
            "https://www.googleapis.com/calendar/v3/calendars/primary/events?alt=json",
            "primary"
        ));
    }

    #[test]
    fn rejects_non_google_or_different_calendar_resources() {
        assert!(!equivalent(
            "https://www.googleapis.com/calendar/v3/calendars/a/events",
            "https://evil.example/calendar/v3/calendars/primary/events",
            "a"
        ));
        assert!(!equivalent(
            "https://www.googleapis.com/calendar/v3/calendars/a/events",
            "https://www.googleapis.com/calendar/v3/calendars/b/events",
            "a"
        ));
        assert!(!equivalent(
            "https://www.googleapis.com/calendar/v3/calendars/a/events",
            "https://www.googleapis.com/calendar/v3/calendars/primary/events",
            "a"
        ));
    }
}
