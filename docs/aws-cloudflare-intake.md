# AWS / Cloudflare Intake

op_pi daemon accepts push webhooks from AWS and Cloudflare and normalizes
them into typed events that route through the standard router/renderer/sink
pipeline.

## Endpoints

| Endpoint | Source | Event kinds |
|---|---|---|
| `POST /aws/sns` | SNS HTTP(S) subscription (CloudWatch Alarms, Budgets, any topic) | `aws.cloudwatch-alarm`, `aws.sns-notification`, `aws.sns-subscription-confirmation` |
| `POST /aws/eventbridge` | EventBridge API destination (GuardDuty, Health, EC2, any `detail-type`) | `aws.eventbridge.<slugified-detail-type>` |
| `POST /cloudflare` | Cloudflare Notifications generic webhook | `cloudflare.<alert_type>` |
| `POST /cloudflare/logpush?dataset=<name>` | Cloudflare Logpush HTTP destination (NDJSON, gzip OK) | `cloudflare.logpush.<dataset>` |

## Configuration

```toml
[aws]
# SNS TopicArn allowlist. Empty = accept every topic.
topic_allowlist = ["arn:aws:sns:us-east-1:123456789012:op_pi-alarms"]
# Optional shared secret for /aws/eventbridge, checked against the `x-api-key`
# header. EventBridge API destination connections support API-key auth — use it.
webhook_secret = "random-long-secret"

[cloudflare]
# Cloudflare Notifications sends this verbatim in the `cf-webhook-auth` header.
webhook_secret = "random-long-secret"
# Logpush `header_X-Logpush-Secret` destination parameter becomes this header.
logpush_secret = "another-random-secret"
```

Endpoints are closed by default: without the matching secret configured they
return `503` instead of accepting unauthenticated traffic.

The SNS endpoint cannot use header auth (SNS sends none), so every SNS message
is verified with the real SNS signature scheme instead: the `SigningCertURL`
is validated (`https://sns.<region>.amazonaws.com/*.pem` only), the X.509
certificate is fetched once and cached, and the canonical-string RSA/SHA-256
(SignatureVersion 2, SHA-1 for version 1) signature must verify, or the
request is rejected with `403`. The topic allowlist is an additional filter
on top of — never a replacement for — signature verification.

## Behavior notes

- **SNS SubscriptionConfirmation / UnsubscribeConfirmation** are normalized
  into `aws.sns-subscription-confirmation` events (carrying `subscribe_url`)
  instead of being auto-confirmed. They pass the same signature verification;
  confirm subscriptions manually after checking the topic.
- **CloudWatch alarm payloads** are detected by parsing SNS `Message` as JSON
  with an `AlarmName` field; other SNS messages stay generic
  `aws.sns-notification` events.
- **Logpush batches** become one event per batch: `record_count`, `records`
  (capped at 100), `truncated`. Bodies are bounded at 10 MiB raw / 5 MiB
  decompressed. Point high-volume datasets (e.g. `http_requests`) at something
  else; this intake is for actionable records like `firewall_events` and
  `audit_logs_v2`.

## Routing examples

```toml
[[routes]]
event = "aws.cloudwatch-alarm"
sink = "discord"
channel = "OPS_CHANNEL_ID"
format = "alert"

[[routes]]
event = "cloudflare.health_check_status_notification"
sink = "slack"
channel = "C123OPS"
format = "alert"

[[routes]]
event = "aws.eventbridge.guardduty-*"
sink = "localfile"
local_path = "/var/log/op_pi/security.jsonl"
```
