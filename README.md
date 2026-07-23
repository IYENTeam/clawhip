# op-pi

**Operation Pipeline Interface**

> Signals in. Decisions routed. Actions out.

`op-pi` is a local-first operations interface for turning events from development
tools, cloud platforms, and agent runtimes into deliberate deliveries and
operator-visible actions.

It gives operational signals one typed path:

```text
sources and webhooks
        |
        v
normalize -> validate -> route -> render -> deliver
        |                          |
        +---- health + audit ------+
```

Use it when several tools can produce events, several channels can receive
them, and the routing decision should live in one explicit configuration
instead of being scattered across webhook scripts.

## Why op-pi

Operational automation usually starts as a collection of point integrations:

- CI posts directly to one chat channel
- cloud alerts use a different webhook
- local agent sessions have their own hooks
- Git and GitHub monitors run separate scripts
- escalation and approval rules are encoded in message text

`op-pi` replaces those disconnected paths with one interface:

1. **Intake** accepts provider-native events.
2. **Normalization** converts them into typed event envelopes.
3. **Routing** matches event families and payload metadata.
4. **Rendering** turns an event into a destination-specific message.
5. **Delivery** sends to Discord, Slack, local files, or an intentional drop.
6. **Operations** expose health, diagnostics, bounded logs, and dry-run
   explanations.

The result is a pipeline you can inspect and reason about before an event
reaches a person or agent.

## Current compatibility names

The product is now **op-pi**, but this repository is in a compatibility-first
rename phase. The shipped runtime identifiers are still:

| Surface | Current identifier |
| --- | --- |
| Binary | `clawhip` |
| Config file | `~/.clawhip/config.toml` |
| Runtime directory | `~/.clawhip/` |
| Environment prefix | `CLAWHIP_` |
| Cargo package | `clawhip` |

Keeping these identifiers temporarily avoids breaking existing installations,
launch agents, hooks, and persisted state. New documentation uses the op-pi
product name while command examples remain executable against the current
binary.

## Pipeline model

```text
                           +----------------------+
 Git / GitHub ------------>|                      |
 tmux / agent hooks ------>|                      |
 Discord threads --------->|      op-pi intake    |
 AWS / Cloudflare -------->|                      |
 custom HTTP / CLI ------->|                      |
                           +----------+-----------+
                                      |
                                      v
                           +----------------------+
                           | typed event envelope |
                           +----------+-----------+
                                      |
                                      v
                           +----------------------+
                           | filters and policies |
                           +----------+-----------+
                                      |
                     +----------------+----------------+
                     |                |                |
                     v                v                v
                  Discord          Slack          localfile
                                                       |
                                                       v
                                                 audit / replay
```

One event may resolve to zero, one, or many deliveries. Routes are declarative,
ordered, and testable with `clawhip explain`.

## What it connects

### Sources and intake

| Source | Interface | Typical events |
| --- | --- | --- |
| Git | local poller and CLI | commits, branch changes |
| GitHub | API poller, webhook, and CLI | issues, PR state, CI |
| Native agent hooks | Codex and Claude hook bridge | session and tool lifecycle |
| tmux | monitored sessions | keywords, stale sessions, recovery |
| Discord threads | Discord API monitor | thread creation and activity |
| AWS SNS | `POST /aws/sns` | CloudWatch alarms, SNS notifications |
| AWS EventBridge | `POST /aws/eventbridge` | GuardDuty, Health, EC2, custom events |
| Cloudflare Notifications | `POST /cloudflare` | alert policies and health checks |
| Cloudflare Logpush | `POST /cloudflare/logpush` | firewall and audit batches |
| Custom events | CLI and `POST /api/event` | internal operational signals |

### Delivery targets

- Discord channels
- Discord threads
- Discord webhooks
- Slack channels through `chat.postMessage`
- Slack incoming webhooks
- JSONL local files
- explicit `drop` routes

Rendering and transport are separate. The same normalized event can be
formatted differently for an alert channel, a routine channel, and an audit
file.

## Quick start

### Build from the current repository

```bash
git clone https://github.com/IYENTeam/clawhip.git op-pi
cd op-pi
./install.sh --skip-star-prompt
```

The installed command is currently `clawhip`.

### Create a minimal Discord configuration

```toml
# ~/.clawhip/config.toml
[providers.discord]
token = "DISCORD_BOT_TOKEN"
default_channel = "DISCORD_CHANNEL_ID"

[[routes]]
event = "github.*"
sink = "discord"
channel = "DISCORD_CHANNEL_ID"
format = "compact"
```

Start and inspect the daemon:

```bash
clawhip start
clawhip status
curl -fsS http://127.0.0.1:25294/health
```

The default local endpoint is:

```text
http://127.0.0.1:25294
```

## Routing examples

### Send CloudWatch alarms to Slack

```toml
[providers.slack]
token = "xoxb-..."
default_channel = "C_OPERATIONS"

[aws]
topic_allowlist = [
  "arn:aws:sns:us-east-1:123456789012:operations"
]

[[routes]]
event = "aws.cloudwatch-alarm"
sink = "slack"
channel = "C_OPERATIONS"
format = "alert"
```

### Split Cloudflare and AWS events between interfaces

```toml
[aws]
webhook_secret = "EVENTBRIDGE_SHARED_SECRET"

[cloudflare]
webhook_secret = "CLOUDFLARE_NOTIFICATION_SECRET"
logpush_secret = "CLOUDFLARE_LOGPUSH_SECRET"

[[routes]]
event = "aws.eventbridge.guardduty-*"
sink = "discord"
channel = "SECURITY_CHANNEL_ID"
format = "alert"

[[routes]]
event = "cloudflare.health_check_status_notification"
sink = "slack"
channel = "C_EDGE_OPERATIONS"
format = "alert"

[[routes]]
event = "cloudflare.logpush.audit_logs_v2"
sink = "localfile"
local_path = "/var/log/op-pi/cloudflare-audit.jsonl"
```

### Require an operator-visible approval route

```toml
[[routes]]
event = "agent.approval-requested"
filter = { project = "production" }
sink = "discord"
channel = "APPROVAL_CHANNEL_ID"
format = "alert"
```

op-pi routes and records the request. It does not interpret message text as
permission to mutate infrastructure.

## Provider-native agent hooks

Codex and Claude own session launch and hook registration. op-pi is the shared
normalization and routing layer.

```bash
clawhip hooks install --provider codex --scope global
clawhip hooks install --provider claude-code --scope global

clawhip native hook --provider codex --file payload.json
clawhip native hook --provider claude --file payload.json
cat payload.json | clawhip native hook --provider codex
```

Shared hook events include:

- `SessionStart`
- `PreToolUse`
- `PostToolUse`
- `UserPromptSubmit`
- `Stop`

Routing should use stable metadata such as `provider`, `event`, `session_id`,
`repo_name`, `project`, `branch`, and `tool_name` rather than rendered text.

See:

- [`docs/native-event-contract.md`](docs/native-event-contract.md)
- [`docs/event-contract-v1.md`](docs/event-contract-v1.md)

## Cloud intake security

Cloud-facing endpoints are closed by default.

### AWS SNS

- validates `TopicArn` against an optional allowlist
- validates `SigningCertURL` as an HTTPS AWS SNS certificate URL
- disables certificate-fetch redirects
- checks X.509 validity
- caches keys only until the shorter of cache TTL or certificate expiry
- verifies RSA/SHA-1 or RSA/SHA-256 SNS signatures

An allowlist is an additional filter, not a substitute for signature
verification.

### EventBridge and Cloudflare

- return `503` until their shared secret is configured
- use constant-time secret comparison
- reject missing or incorrect authentication

### Cloudflare Logpush

- limits raw request bodies to 10 MiB
- limits decompressed payloads to 5 MiB
- accepts NDJSON and gzip
- caps retained records per batch
- rejects malformed records and decompression bombs

See [`docs/aws-cloudflare-intake.md`](docs/aws-cloudflare-intake.md).

## Explain before dispatch

Use the routing explainer to inspect a decision without sending anything:

```bash
clawhip explain github.pr-status-changed repo=my-app number=42
clawhip explain --json github.pr-status-changed repo=my-app number=42
```

Other useful operational commands:

```bash
clawhip status
clawhip config
clawhip config verify-gateway-allowlist
clawhip send --channel <id> --message "test"
clawhip plugin list
clawhip tmux list
clawhip gajae status
```

## Reliability and observability

- bounded internal queue
- typed event normalization
- route validation at startup
- source health in `/health`
- retry handling for Slack rate limits and server failures
- degraded-source backoff
- repeated-error log deduplication
- bounded log rotation utility

The bundled rotation utility keeps operational logs bounded:

```bash
scripts/rotate-clawhip-logs.sh
```

Defaults:

- rotate above 25 MiB
- retain four gzip generations
- restart the launchd service after rotating open log files

The macOS deployment can schedule it through a user LaunchAgent. Other service
managers may use their native rotation facilities.

## Configuration principles

1. Keep secrets in provider configuration or environment variables.
2. Match routes on typed event fields, not message text.
3. Use explicit Discord channel/thread targets.
4. Keep dynamic tokens disabled unless a route requires them.
5. Route high-volume datasets to local files before chat.
6. Use `drop` intentionally for acknowledged noise.
7. Treat approval events as requests, not authorization.

## Repository map

```text
src/source/       event producers and monitors
src/intake.rs     AWS and Cloudflare normalization and authentication
src/router.rs     route resolution
src/render/       destination-independent rendering
src/sink/         Discord, Slack, and local-file delivery
src/daemon.rs     HTTP server, queue, source lifecycle, health
docs/             event contracts and operational runbooks
scripts/          verification and operations utilities
plugins/          tool-specific hook bridges
```

Architecture details:

- [`ARCHITECTURE.md`](ARCHITECTURE.md)
- [`docs/live-verification.md`](docs/live-verification.md)
- [`docs/agent-runbook.md`](docs/agent-runbook.md)

## Project direction

op-pi is an independent operations pipeline, not an extension of an upstream
product. Its design center is:

- explicit operational interfaces
- provider-native event intake
- deterministic routing
- human-visible policy boundaries
- local-first operation
- secure cloud ingress
- inspectable delivery behavior

The current compatibility identifiers will remain documented until the runtime,
package, config path, and service names are migrated together.

## License

MIT. See [`LICENSE`](LICENSE).
