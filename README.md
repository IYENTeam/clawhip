<p align="center">
  <img src="assets/op_pi-hero.svg" width="100%" alt="op_pi routes operational signals through one typed pipeline to Discord, Slack, and local interfaces" />
</p>

<h1 align="center">op_pi</h1>

<p align="center">
  <strong>Operation Pipeline Interface</strong>
  <br />
  One operational path from every signal to the right human, agent, or system.
</p>

<p align="center">
  <a href="https://github.com/IYENTeam/op_pi/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/IYENTeam/op_pi/ci.yml?branch=main&style=flat-square&label=build&labelColor=111827&color=55e6c1" alt="Build status" /></a>
  <img src="https://img.shields.io/badge/pipeline-local--first-8b7cff?style=flat-square&labelColor=111827" alt="Local-first pipeline" />
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-4aa8ff?style=flat-square&labelColor=111827" alt="MIT License" /></a>
  <img src="https://img.shields.io/badge/Rust-2024-f97316?style=flat-square&labelColor=111827&logo=rust&logoColor=white" alt="Rust 2024" />
</p>

<p align="center">
  <a href="#quick-start">Quick start</a>
  &nbsp;&middot;&nbsp;
  <a href="#one-pipeline-not-another-webhook-script">Why op_pi</a>
  &nbsp;&middot;&nbsp;
  <a href="#what-it-connects">Interfaces</a>
  &nbsp;&middot;&nbsp;
  <a href="#cloud-intake-security">Security</a>
  &nbsp;&middot;&nbsp;
  <a href="#operations">Operations</a>
</p>

---

## Events happen everywhere. Operations should not.

CI failures, cloud alerts, GitHub activity, agent hooks, and live tmux sessions
all speak different protocols. Teams usually connect them with one-off webhook
scripts until nobody can explain why an alert reached one channel, missed
another, or triggered an action.

**op_pi gives those signals one typed, inspectable path.**

```text
intake  ->  normalize  ->  validate  ->  route  ->  render  ->  deliver
```

<table>
  <tr>
    <td width="25%" valign="top">
      <strong>Provider-native intake</strong><br />
      GitHub, Git, AWS, Cloudflare, Discord, tmux, Codex, Claude, and custom events.
    </td>
    <td width="25%" valign="top">
      <strong>Typed routing</strong><br />
      Match event families and payload metadata instead of parsing message text.
    </td>
    <td width="25%" valign="top">
      <strong>Interface-aware delivery</strong><br />
      Render once, then deliver to Discord, Slack, local files, or an intentional drop.
    </td>
    <td width="25%" valign="top">
      <strong>Operational control</strong><br />
      Explain decisions, inspect source health, bound logs, and keep cloud ingress closed by default.
    </td>
  </tr>
</table>

## Quick start

### Install from the current repository

```bash
git clone https://github.com/IYENTeam/op_pi.git
cd op_pi
./install.sh --skip-star-prompt
```

The installed command is `op_pi`.

### Route your first event

```toml
# ~/.op_pi/config.toml
[providers.discord]
token = "DISCORD_BOT_TOKEN"
default_channel = "DISCORD_CHANNEL_ID"

[[routes]]
event = "github.*"
sink = "discord"
channel = "DISCORD_CHANNEL_ID"
format = "compact"
```

```bash
op_pi start
op_pi status
op_pi explain github.pr-status-changed repo=my-app number=42
```

The daemon listens locally at `http://127.0.0.1:25294`.

```bash
curl -fsS http://127.0.0.1:25294/health
```

## One pipeline, not another webhook script

| Point integration | op_pi |
| --- | --- |
| Each provider owns its own delivery logic | Providers only produce normalized events |
| Message text becomes accidental routing state | Typed fields and explicit filters decide routes |
| One input maps to one hard-coded destination | One event resolves to zero, one, or many deliveries |
| Authentication varies by copied script | Intake security is centralized and testable |
| Failures repeat forever in unbounded logs | Health, backoff, deduplication, and rotation are built in |
| Nobody can preview a routing decision | `op_pi explain` shows the route without dispatching |

## The operation pipeline

```mermaid
flowchart LR
    subgraph Sources
        GH[GitHub + Git]
        AG[Agent hooks]
        CL[Cloud events]
        TM[tmux + Discord]
    end

    subgraph op_pi
        IN[Intake]
        EN[Typed envelope]
        RT[Routes + policy]
        RD[Renderer]
        IN --> EN --> RT --> RD
    end

    subgraph Interfaces
        DC[Discord]
        SL[Slack]
        LF[Local JSONL]
        DR[Drop]
    end

    GH --> IN
    AG --> IN
    CL --> IN
    TM --> IN
    RD --> DC
    RD --> SL
    RD --> LF
    RD --> DR
```

One event can fan out across interfaces. Rendering and transport stay separate,
so the same operational fact can be compact in a routine channel, prominent in
an escalation channel, and complete in an audit file.

## What it connects

### Intake surfaces

| Source | Interface | Typical events |
| --- | --- | --- |
| Git | local poller and CLI | commits, branch changes |
| GitHub | API poller, webhook, and CLI | issues, pull requests, CI |
| Codex and Claude | provider-native hook bridge | session and tool lifecycle |
| tmux | monitored sessions | keywords, stale sessions, recovery |
| Discord threads | Discord API monitor | thread creation and activity |
| AWS SNS | `POST /aws/sns` | CloudWatch alarms, SNS notifications |
| AWS EventBridge | `POST /aws/eventbridge` | GuardDuty, Health, EC2, custom events |
| Cloudflare Notifications | `POST /cloudflare` | alert policies and health checks |
| Cloudflare Logpush | `POST /cloudflare/logpush` | firewall and audit batches |
| Google Calendar | `POST /google/calendar` | watch synchronization and resource changes |
| Custom systems | CLI and `POST /api/event` | internal operational signals |

### Delivery interfaces

| Interface | Targets | Behavior |
| --- | --- | --- |
| Discord | channels, threads, webhooks | explicit targets, rate-limit handling |
| Slack | channels and incoming webhooks | Block Kit rendering, 429 and 5xx retry |
| Local files | JSONL paths | durable audit, replay, and high-volume capture |
| Drop | explicit route | acknowledge noise without accidental delivery |

## Route by intent

### CloudWatch alarm to Slack

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

### Security events to Discord, audit events to disk

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
local_path = "/var/log/op_pi/cloudflare-audit.jsonl"
```

### Google Calendar changes to Slack

```toml
[providers.slack]
bot_token = "xoxb-your-slack-bot-token"

[google_calendar]
channel_token = "replace-with-a-random-channel-token"
credentials_file = "/Users/you/.op_pi/google-calendar-oauth.json"
state_file = "/Users/you/.op_pi/google-calendar-state.json"
calendar_id = "primary"
callback_url = "https://ops.example.com/google/calendar"
renewal_margin_secs = 86400

[[routes]]
event = "calendar.*"
sink = "slack"
channel = "C_CALENDAR_OPERATIONS"
format = "compact"
```

### Approval requests stay requests

```toml
[[routes]]
event = "agent.approval-requested"
filter = { project = "production" }
sink = "discord"
channel = "APPROVAL_CHANNEL_ID"
format = "alert"
```

op_pi routes and records the request. It does not treat message text as
authorization to mutate infrastructure.

## Provider-native agent hooks

Codex and Claude own session launch and hook registration. op_pi owns the
shared event contract, normalization, and routing layer.

```bash
op_pi hooks install --provider codex --scope global
op_pi hooks install --provider claude-code --scope global

op_pi native hook --provider codex --file payload.json
op_pi native hook --provider claude --file payload.json
cat payload.json | op_pi native hook --provider codex
```

Shared hook events include `SessionStart`, `PreToolUse`, `PostToolUse`,
`UserPromptSubmit`, and `Stop`.

Route with stable metadata such as `provider`, `event`, `session_id`,
`repo_name`, `project`, `branch`, and `tool_name` rather than rendered text.

- [Native event routing guide](docs/native-event-contract.md)
- [Frozen v1 event contract](docs/event-contract-v1.md)

## Cloud intake security

> [!NOTE]
> Cloud-facing endpoints are closed until their authentication settings are
> explicitly configured.

<details>
<summary><strong>AWS SNS signature verification</strong></summary>

<br />

- validates `TopicArn` against an optional allowlist
- restricts `SigningCertURL` to HTTPS AWS SNS certificate endpoints
- disables certificate-fetch redirects
- checks X.509 validity
- caps key caching at the shorter of cache TTL or certificate expiry
- verifies RSA/SHA-1 and RSA/SHA-256 SNS signatures

An allowlist is an additional filter, not a substitute for signature
verification.

</details>

<details>
<summary><strong>EventBridge and Cloudflare authentication</strong></summary>

<br />

- return `503` until a shared secret is configured
- compare secrets in constant time
- reject missing or incorrect authentication

</details>

<details>
<summary><strong>Cloudflare Logpush bounds</strong></summary>

<br />

- limits raw request bodies to 10 MiB
- limits decompressed payloads to 5 MiB
- accepts NDJSON and gzip
- caps retained records per batch
- rejects malformed records and decompression bombs

</details>

<details>
<summary><strong>Google Calendar channel authentication</strong></summary>

<br />

- returns `503` until `[google_calendar].channel_token` is configured
- compares `X-Goog-Channel-Token` in constant time
- validates all required `X-Goog-*` notification headers
- binds accepted notifications to tracked channel/resource identities while
  supporting Google's `primary`/canonical-account URI alias
- prevents general `/event` traffic from triggering Calendar API synchronization
- uses external mode-0600 OAuth credentials with Calendar read-only scope
- performs retrying full/incremental sync with durable webhook triggers,
  sink-confirmed outbox delivery, and HTTP 410 recovery
- renews expiring watch channels, bounds pending activation, and durably retires old channels
- deduplicates notification message numbers across daemon restarts
- emits typed created, updated, cancelled, and failure events
- exposes separate public-safe sync/watch health without credential or token values

</details>

Read the complete [AWS and Cloudflare intake guide](docs/aws-cloudflare-intake.md).
For Calendar watch setup and event fields, see the
[Google Calendar intake guide](docs/google-calendar-intake.md).

## Explain before dispatch

Inspect a routing decision without sending anything:

```bash
op_pi explain github.pr-status-changed repo=my-app number=42
op_pi explain --json github.pr-status-changed repo=my-app number=42
```

The explainer reports which routes match, which filters pass, and where each
delivery would go.

## Operations

```bash
op_pi status
op_pi config
op_pi config verify-gateway-allowlist
op_pi send --channel <id> --message "test"
op_pi plugin list
op_pi tmux list
op_pi gajae status
```

Operational behavior includes:

- bounded internal queues
- startup route validation
- source health in `/health`
- Slack rate-limit and server-failure retries
- degraded-source backoff
- repeated-error log deduplication
- bounded log rotation

```bash
scripts/rotate_op_pi_logs.sh
```

The bundled utility rotates above 25 MiB, retains four gzip generations, and
can restart the launchd service after rotating open log files.

## Configuration principles

1. Keep secrets in provider configuration or environment variables.
2. Match typed event fields, never rendered message text.
3. Use explicit Discord channel or thread targets.
4. Keep dynamic tokens disabled unless a route requires them.
5. Route high-volume datasets to local files before chat.
6. Use `drop` only for intentionally acknowledged noise.
7. Treat approval events as requests, not authorization.

## Identity

op_pi is the sole identifier for commands, configuration, automation,
deployments, and telemetry. The versioned telemetry schema is
`op_pi.telemetry.v1`.

## Repository map

```text
src/source/       event producers and monitors
src/intake.rs     AWS, Cloudflare, and Google Calendar intake authentication
src/router.rs     route resolution
src/render/       destination-independent rendering
src/sink/         Discord, Slack, and local-file delivery
src/daemon.rs     HTTP server, queue, source lifecycle, health
docs/             event contracts and operational runbooks
scripts/          verification and operations utilities
plugins/          tool-specific hook bridges
```

### Read next

- [Architecture](ARCHITECTURE.md)
- [Live verification runbook](docs/live-verification.md)
- [Agent operations runbook](docs/agent-runbook.md)
- [Memory offload architecture](docs/memory-offload-architecture.md)

## Design center

op_pi is an independent operations pipeline built around:

- explicit operational interfaces
- provider-native event intake
- deterministic, inspectable routing
- human-visible policy boundaries
- local-first operation
- secure cloud ingress
- bounded delivery behavior

---

<p align="center">
  <strong>Signals in. Decisions routed. Actions out.</strong>
  <br />
  <sub>op_pi is licensed under the <a href="LICENSE">MIT License</a>.</sub>
</p>
