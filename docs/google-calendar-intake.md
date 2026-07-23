# Google Calendar intake

`op_pi` accepts Google Calendar API push notifications at:

```text
POST https://YOUR_OP_PI_HOST/google/calendar
```

Google requires the callback to use HTTPS with a valid public certificate.
`op_pi` receives notifications for Calendar API resources that support
`watch`, including events, ACLs, calendar lists, and settings.

## Configure authentication

Generate a random channel token and configure it in `op_pi`:

```toml
[google_calendar]
channel_token = "replace-with-a-random-channel-token"
```

Use the same token when creating the Google Calendar watch:

```http
POST https://www.googleapis.com/calendar/v3/calendars/CALENDAR_ID/events/watch
Authorization: Bearer GOOGLE_OAUTH_ACCESS_TOKEN
Content-Type: application/json

{
  "id": "a-unique-channel-id",
  "type": "web_hook",
  "address": "https://YOUR_OP_PI_HOST/google/calendar",
  "token": "replace-with-a-random-channel-token"
}
```

Google includes that token in `X-Goog-Channel-Token`. The endpoint stays closed
with HTTP `503` until a token is configured, and rejects missing or incorrect
tokens with HTTP `401`.

## Events

| Google resource state | `op_pi` event |
|---|---|
| `sync` | `google.calendar.sync` |
| `exists` | `google.calendar.changed` |

The normalized payload contains:

```json
{
  "summary": "Google Calendar resource changed",
  "source": "google.calendar",
  "channel_id": "a-unique-channel-id",
  "resource_id": "opaque-google-resource-id",
  "resource_uri": "https://www.googleapis.com/calendar/v3/calendars/example/events",
  "resource_state": "exists",
  "message_number": 42,
  "channel_expiration": "Tue, 19 Nov 2030 01:13:52 GMT"
}
```

`channel_expiration` is included only when Google sends the corresponding
header.

## Route notifications

```toml
[[routes]]
event = "google.calendar.changed"
sink = "slack"
channel = "C0123456789"
format = "compact"
```

Use `google.calendar.sync` for channel initialization observability and
`google.calendar.changed` to trigger downstream synchronization.

## Important API behavior

Google Calendar notifications have no request body and do not identify which
calendar event changed. They only signal that the watched resource changed.
Fetch actual changes with the Calendar API, normally by calling `events.list`
with a stored sync token.

The initial `sync` notification can arrive before the `watch` response because
of network timing. Message numbers increase for a channel but are not
guaranteed to be sequential. Notification channels also expire and must be
renewed by creating a new watch.

Treat notifications as at-least-once signals. `op_pi` preserves Google's
message number but does not persist a per-channel deduplication cursor, so the
downstream Calendar API synchronization must be idempotent.

Official reference:
[Google Calendar API push notifications](https://developers.google.com/workspace/calendar/api/guides/push).
