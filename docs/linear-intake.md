# Linear Intake

op_pi daemon accepts signed Linear webhooks at `POST /linear` and normalizes every accepted Linear event into the same routing pipeline used by other intake sources.

Linear expects a public HTTPS endpoint that answers with HTTP `200` within 5 seconds. Deploy op_pi behind your own TLS terminator or reverse proxy; op_pi does not provision tunnels or public HTTPS. Linear's webhook documentation is at <https://linear.app/developers/webhooks>.

## Endpoint and configuration

```toml
[linear]
webhook_secret = "LINEAR_WEBHOOK_SECRET"

[[routes]]
event = "linear.issue-label-update"
sink = "discord"
channel = "TRIAGE_CHANNEL_ID"
format = "compact"
```

`[linear].webhook_secret` is optional. When absent or blank, `POST /linear` is closed and returns `503 Service Unavailable` without reading, authenticating, or parsing the request body.

Configure the same secret in Linear's webhook settings and op_pi. A Linear-enabled config must be a private regular file; op_pi rejects symlinks or group/world-readable files and saves it atomically as mode `0600`. `op_pi config show` is display-only redacted output and is not round-trippable. The endpoint accepts only `POST /linear`; `GET /linear` returns `405 Method Not Allowed`, and `/api/linear` is not an alias.

## Signature and replay checks

Every configured request must include `Linear-Signature`. op_pi hex-decodes that header as a 32-byte HMAC-SHA256 and verifies it over the exact raw request body bytes using `[linear].webhook_secret`. Any body byte change, including JSON whitespace, changes the MAC.

Authentication happens before JSON parsing. Missing, non-hex, wrong-length, or mismatched signatures return `401 Unauthorized`, even if the body is invalid JSON.

After signature verification, op_pi requires the signed top-level JSON object to contain `webhookTimestamp` as an unsigned integer within 60 seconds of the daemon clock (`<= 60,000 ms` difference). Missing, negative, fractional, string, boolean, or null timestamps return `400 Bad Request`; stale or too-far-future signed timestamps return `401 Unauthorized`.

The raw request body is capped at 1 MiB. With a configured secret, bodies up to exactly 1,048,576 bytes can be accepted; 1,048,577 bytes returns `413 Payload Too Large` before signature verification.

## Status contract and precedence

op_pi checks configuration first, then rate limit (`429`) and body-read concurrency (`503`) before reading the body. Those admission controls may therefore precede body errors. It then enforces the body-read timeout/stream contract (`400`) and body size (`413`), verifies the HMAC (`401`), checks `Linear-Delivery` length (`400`), validates the signed JSON and timestamp (`400`/`401`), checks replay-cache capacity (`503`), and attempts queue admission (`503`).

| Condition | Status |
| --- | --- |
| `[linear].webhook_secret` absent or blank | `503 Service Unavailable` |
| configured request body exceeds 1 MiB | `413 Payload Too Large` |
| missing, malformed, wrong-length, or mismatched `Linear-Signature` | `401 Unauthorized` |
| `Linear-Delivery` exceeds 1,024 bytes | `400 Bad Request` |
| signed body is not a JSON object or has invalid `type`, `action`, or timestamp field type | `400 Bad Request` |
| signed `webhookTimestamp` is more than 60 seconds from the daemon clock | `401 Unauthorized` |
| concurrent body-read or replay-cache capacity is exhausted; queue is full or closed | `503 Service Unavailable` |
| rate limit is exhausted | `429 Too Many Requests` |
| body read times out or body stream fails | `400 Bad Request` |
| event validates and is admitted to the queue, or is a bounded replay duplicate | `200 OK` |

`200 OK` means the request was newly admitted to the local queue or is a duplicate of a previously admitted request suppressed by the replay cache. Non-`200` responses do not enqueue an event.

## Event naming and payload

For each signed object with nonempty `type` and `action`, op_pi trims both fields, converts each component to kebab case, and emits:

```text
linear.<type-kebab>-<action-kebab>
```

Examples:

| Signed `type` | Signed `action` | op_pi event |
| --- | --- | --- |
| `IssueLabel` | `update` | `linear.issue-label-update` |
| `IssueSLA` | `highRisk` | `linear.issue-sla-high-risk` |

This mapping is generic: any signed `type`/`action` pair whose normalized components are nonempty can route.

The accepted event payload has this shape:

```json
{
  "webhook": {
    "action": "update",
    "type": "IssueLabel",
    "webhookTimestamp": 1700000000000,
    "data": {"id": "lbl_1"}
  },
  "linear_delivery": "opaque-delivery-id",
  "event_id": "generated-uuid",
  "correlation_id": "generated-uuid"
}
```

`payload.webhook` is the exact signed JSON object after parsing. `Linear-Delivery`
is copied to `payload.linear_delivery` only when valid UTF-8. It is opaque
correlation metadata only: op_pi does not authenticate with it or derive event
kinds from it. op_pi injects matching `event_id` and `correlation_id` UUIDs for
accepted queue and telemetry correlation.

## Delivery guarantees and retries

Linear may retry non-`200` responses. Intake is best-effort and volatile:

- duplicate deliveries can receive `200 OK` without a second enqueue while their verified raw body remains in the bounded in-memory replay cache;
- replay entries expire at signed `webhookTimestamp + 60 seconds`, cache capacity is bounded, and state is lost on restart;
- post-`200` loss is possible if the daemon exits after queue admission but before downstream delivery; and
- there is no persistent or exactly-once delivery guarantee.

Localfile delivery renders and may truncate event content; it is not an authoritative audit trail. Preserve the original Linear delivery at an external durable system when authoritative retention is required.

## Reverse proxy controls

Require the TLS terminator or reverse proxy to enforce a 16 KiB total-header limit, exact 64-character hex signature header, 1,024-byte `Linear-Delivery` maximum, 5-second header/read/idle timeouts, and a 32-connection/concurrent-read cap. Configure 64 requests/second with burst 128. The application enforces the 32 body-read semaphore, 5-second body-read timeout, delivery cap, and rate response, but cannot protect pre-header parsing; proxy enforcement is required. Do not decompress, reserialize, normalize, or otherwise alter the request body before forwarding it: Linear's HMAC covers the exact raw bytes.

## Out of scope

The Linear intake does not provide Linear polling, API tokens, OAuth, webhook provisioning, persistent dedupe storage, or public tunnel/reverse-proxy provisioning.
