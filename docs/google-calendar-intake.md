# Google Calendar intake

`op_pi` accepts Google Calendar API push notifications at:

```text
POST https://YOUR_OP_PI_HOST/google/calendar
```

Google requires the callback to use HTTPS with a valid public certificate.
`op_pi` receives notifications for Calendar API resources that support
`watch`, including events, ACLs, calendar lists, and settings.

## Configure synchronization

Authorize Google Calendar with the read-only
`https://www.googleapis.com/auth/calendar.events.readonly` scope. Every other
Google Calendar scope, including `calendar.readonly` and writable scopes, is
rejected. Only `openid`, `email`, and `userinfo.email` identity scopes are
allowed alongside the required Calendar scope.
Store the resulting authorized-user JSON outside the TOML file and restrict it
to the daemon user:

```bash
chmod 600 ~/.op_pi/google-calendar-oauth.json
```

Generate a random channel token and configure the source:

```toml
[google_calendar]
channel_token = "replace-with-a-random-channel-token"
credentials_file = "/Users/you/.op_pi/google-calendar-oauth.json"
state_file = "/Users/you/.op_pi/google-calendar-state.json"
calendar_id = "primary"
callback_url = "https://YOUR_OP_PI_HOST/google/calendar"
renewal_margin_secs = 86400
```

`credentials_file`, `state_file`, `callback_url`, and `channel_token` are
required together. A durable-sync `channel_token` must contain at least 32
bytes. Credential files readable by group or other users are rejected. OAuth
client secrets and refresh tokens are never serialized into the op_pi config or
status output. Because an inline channel token is a secret, its config file
must also be a regular file accessible only by its owner:

```bash
chmod 600 ~/.op_pi/config.toml
```

### Webhook-only migration

PR #17 webhook-only configurations that set only `channel_token` remain
compatible, including existing tokens shorter than 32 bytes. Before adding
`credentials_file`, `state_file`, and `callback_url` to enable durable sync,
replace a short token with a new random value of at least 32 bytes, then ensure
the config is mode `0600`. Configurations without an inline channel token may
remain mode `0644`; loading never changes their permissions.

Release builds accept only the official Google Calendar API and OAuth token
origins. The callback must be HTTPS and cannot contain userinfo, a query, or a
fragment.

op_pi refreshes the OAuth access token, performs the initial `events.list`,
stores the final `nextSyncToken`, and creates `events.watch` automatically.
Google includes the configured token in `X-Goog-Channel-Token`. The endpoint
stays closed with HTTP `503` until a token is configured, and rejects missing
or incorrect tokens with HTTP `401`.

## Events

| Google resource state | `op_pi` event |
|---|---|
| `sync` | `google.calendar.sync` |
| `exists` | `google.calendar.changed` |

Incremental synchronization emits:

| Calendar change | `op_pi` event |
|---|---|
| new event | `calendar.event.created` |
| changed event | `calendar.event.updated` |
| cancelled/deleted event | `calendar.event.cancelled` |
| OAuth/state bootstrap failure | `calendar.sync.bootstrap-failed` |
| initial baseline failure | `calendar.sync.initial-failed` |
| sync failure | `calendar.sync.failed` |
| HTTP 410 baseline recovery failure | `calendar.sync.recovery-failed` |
| watch renewal failure | `calendar.watch.renewal-failed` |
| watch activation timeout | `calendar.watch.activation-timed-out` |
| superseded watch stop failure | `calendar.watch.stop-failed` |

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
event = "calendar.*"
sink = "slack"
channel = "C0123456789"
format = "compact"
```

The same event pattern can target Discord. Created, updated, and cancelled
events render the title, start/end time, attendees, and Meet or Calendar link
when Google supplies them. Calendar messages are Unicode-safely bounded to
1,900 characters for Discord and Slack compatibility.

## Durable synchronization and renewal

- Initial full sync establishes a baseline and does not flood destinations with
  historical events.
- Incremental pages use the stored `nextSyncToken`; only the final page advances
  the cursor.
- HTTP `410 Gone` clears the stale cursor and rebuilds the baseline.
- Initial, incremental, and recovery failures use bounded backoff and retry
  without waiting for another webhook or restarting the daemon.
- Event changes enter a persisted outbox in the same atomic state write as the
  new cursor and message number. Calendar deliveries bypass routine batching,
  and the outbox entry is removed only after all resolved sinks report success.
  Sink failure or daemon restart retains and retries the same event, providing
  at-least-once rather than at-most-once delivery.
- The webhook returns `202` only after its synchronization trigger is durably
  represented. A crash after callback acceptance therefore resumes from
  persisted state instead of waiting for another Google notification.
- `(channel_id, message_number)` progress is persisted and bounded, preventing
  duplicate sync calls across retries and daemon restarts.
- Only the private authenticated webhook path can trigger synchronization.
  Channel ID and resource ID must exactly match a tracked active or pending
  watch. The resource URI must be the corresponding official Google Calendar
  events collection; Google may represent the same primary calendar as either
  `primary` or its percent-encoded account address only when `calendar_id` is
  configured as `primary`. Explicit calendar IDs require an exact ID match.
- Watch renewal sleeps until `expiration - renewal_margin_secs`; it does not
  poll or renew daily.
- A replacement channel remains pending until its `sync` notification arrives.
  op_pi then activates it and stops the previous channel.
- A pending channel that does not acknowledge `sync` within five minutes is
  retired and replaced before the active channel expires.
- Superseded channels remain durably recorded until `channels.stop` succeeds;
  stop failures retry across daemon restarts.
- Renewal and stop failures preserve the still-valid active channel and emit
  sanitized alerts without exposing API URLs, sync tokens, or page tokens.

The state file must be a private regular file in a non-group-writable,
non-world-writable real directory. Writes use a unique mode-`0600` temporary
file, file fsync, atomic rename, and parent-directory fsync.

## Operations

`GET /api/status` and `GET /health` expose public-safe Calendar fields under
`sources.google-calendar.details`:

- cursor presence
- active and pending channel IDs and expiration times
- retiring channel IDs and persisted outbox depth
- last notification time and message number
- last successful sync time
- separate sanitized sync and watch errors

OAuth, Calendar API, watch, and stop requests have bounded connect and
whole-request timeouts so a hung upstream cannot park synchronization or
renewal indefinitely.

OAuth access tokens, refresh tokens, client secrets, channel tokens, and sync
tokens are never included.

Google Workspace session-control policy can invalidate an otherwise durable
refresh token with `invalid_rapt`. In that case, reauthorize the Victor account,
replace the mode-`0600` credential file, and restart the daemon. Persisted
failed sync work resumes automatically from the same cursor after restart.

## Important API behavior

Google Calendar notifications have no request body and do not identify which
calendar event changed. They only signal that the watched resource changed.
Fetch actual changes with the Calendar API, normally by calling `events.list`
with a stored sync token.

The initial `sync` notification can arrive before the `watch` response because
of network timing. Message numbers increase for a channel but are not
guaranteed to be sequential. Notification channels also expire and must be
renewed by creating a new watch.

Treat notifications as at-least-once signals. op_pi persists monotonic
per-channel progress, Calendar event revisions, and an outbox, but downstream
destinations should still treat delivery as idempotent.

Official reference:
[push notifications](https://developers.google.com/workspace/calendar/api/guides/push),
[incremental synchronization](https://developers.google.com/workspace/calendar/api/guides/sync),
and [`events.list`](https://developers.google.com/workspace/calendar/api/v3/reference/events/list).
