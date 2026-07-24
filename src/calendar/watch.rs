use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::Result;
use crate::calendar::state::WatchChannel;

pub const REQUESTED_WATCH_LIFETIME_SECS: u64 = 7 * 24 * 60 * 60;
const WATCH_LIFETIME_MILLIS: i64 = (REQUESTED_WATCH_LIFETIME_SECS as i64) * 1000;

pub fn now_millis() -> Result<i64> {
    Ok(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}

pub fn requested_expiration_ms(now_ms: i64) -> i64 {
    now_ms.saturating_add(WATCH_LIFETIME_MILLIS)
}

pub fn renewal_due(
    active: Option<&WatchChannel>,
    pending: Option<&WatchChannel>,
    now_ms: i64,
    margin_secs: u64,
) -> bool {
    if let Some(watch) = pending {
        return !(watch.expiration_ms > now_ms
            && watch
                .activation_deadline_ms
                .is_none_or(|deadline| deadline > now_ms));
    }
    let margin_ms = i64::try_from(margin_secs)
        .unwrap_or(i64::MAX)
        .saturating_mul(1000);
    active.is_none_or(|watch| watch.expiration_ms <= now_ms.saturating_add(margin_ms))
}

pub fn renewal_delay(
    active: Option<&WatchChannel>,
    pending: Option<&WatchChannel>,
    now_ms: i64,
    margin_secs: u64,
) -> Duration {
    let margin_ms = i64::try_from(margin_secs)
        .unwrap_or(i64::MAX)
        .saturating_mul(1000);
    let deadline = pending
        .and_then(|watch| {
            watch
                .activation_deadline_ms
                .or(Some(watch.expiration_ms))
                .filter(|deadline| *deadline > now_ms)
        })
        .or_else(|| active.map(|watch| watch.expiration_ms.saturating_sub(margin_ms)))
        .unwrap_or(now_ms);
    Duration::from_millis(u64::try_from(deadline.saturating_sub(now_ms)).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::{
        REQUESTED_WATCH_LIFETIME_SECS, renewal_delay, renewal_due, requested_expiration_ms,
    };
    use crate::calendar::state::WatchChannel;
    use std::time::Duration;

    #[test]
    fn requested_expiration_uses_the_fixed_seven_day_lifetime() {
        assert_eq!(
            requested_expiration_ms(1_000),
            1_000 + (REQUESTED_WATCH_LIFETIME_SECS as i64 * 1_000)
        );
        assert_eq!(REQUESTED_WATCH_LIFETIME_SECS, 7 * 24 * 60 * 60);
    }

    #[test]
    fn pending_activation_deadline_drives_replacement() {
        let active = watch("active", 200_000, None);
        let pending = watch("pending", 300_000, Some(99_999));
        assert!(renewal_due(Some(&active), Some(&pending), 100_000, 0));
        assert_eq!(
            renewal_delay(Some(&active), Some(&pending), 100_000, 0),
            Duration::from_secs(100)
        );
    }

    fn watch(id: &str, expiration_ms: i64, activation_deadline_ms: Option<i64>) -> WatchChannel {
        WatchChannel {
            id: id.to_string(),
            resource_id: format!("{id}-resource"),
            resource_uri: "https://example.test/events".to_string(),
            expiration_ms,
            activation_deadline_ms,
        }
    }
}
