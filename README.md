<p align="center">
  <img src="assets/clawhip-mascot.jpg" alt="clawhip mascot" width="420">
</p>

<h1 align="center">🦞🔥 clawhip</h1>

<p align="center">
  <strong>Daemon-first event gateway for Discord</strong><br>
  GitHub webhooks, git polling, tmux monitoring, and CLI clients all flow through one daemon.
</p>

---

## What clawhip is

`clawhip` is a **daemon**, not a one-shot direct-to-Discord CLI.

The daemon listens on **port `25294` by default** and is the **single delivery point** for all notifications.

Architecture:

```text
[CLI client] --------------------------->
[GitHub webhook] ----------------------->
[Built-in git monitor] ----------------->  [clawhip daemon :25294] -> [route/filter engine] -> [Discord REST API]
[Built-in tmux monitor] --------------->
[clawhip tmux new registration] ------->
```

Everything routes through the daemon.

## Core model

- `clawhip` or `clawhip start` starts the daemon
- `clawhip status` checks daemon health
- `clawhip send`, `clawhip github ...`, `clawhip git ...`, `clawhip tmux keyword|stale ...` are **thin clients**
- thin clients POST events to the local daemon API
- the daemon performs all Discord delivery
- the daemon also runs built-in:
  - git repo polling
  - tmux keyword monitoring
  - tmux staleness monitoring

## Default port

Default daemon port: **`25294`**

Why `25294`?
- `CLAW` on a phone keypad -> `2529`
- plus `4` for the whip

Default daemon base URL:

```text
http://127.0.0.1:25294
```

## Commands

### Start the daemon

```bash
# default daemon mode
clawhip

# explicit
clawhip start

# override port
clawhip start --port 25294
```

### Check daemon health

```bash
clawhip status
```

### Send events via daemon client commands

These commands do **not** talk to Discord directly. They POST to the running daemon.

```bash
# custom event
clawhip send --channel 1468539002985644084 --message "Build complete"

# git events
clawhip git commit \
  --repo clawhip \
  --branch main \
  --commit deadbeefcafebabe \
  --summary "Ship daemon refactor"

clawhip git branch-changed \
  --repo clawhip \
  --old-branch feature/x \
  --new-branch main

# github events
clawhip github issue-opened \
  --repo clawhip \
  --number 42 \
  --title "Webhook regression"

clawhip github pr-status-changed \
  --repo clawhip \
  --number 77 \
  --title "Add daemon mode" \
  --old-status open \
  --new-status merged \
  --url https://github.com/Yeachan-Heo/clawhip/pull/77

# tmux event clients
clawhip tmux keyword \
  --session issue-1440 \
  --keyword "PR created" \
  --line "PR #1453 created"

clawhip tmux stale \
  --session issue-1440 \
  --pane 0.0 \
  --minutes 10 \
  --last-line "running integration tests"
```

## Built-in monitoring

The daemon includes built-in monitors configured in `~/.clawhip/config.toml`.

### Git monitoring

The daemon can poll repositories for:
- new commits
- branch changes
- PR status changes

### tmux monitoring

The daemon can monitor tmux sessions for:
- keyword matches in pane output
- stale panes with no new output for N minutes

## tmux wrapper

`clawhip tmux new` launches tmux locally, then registers the session with the daemon for monitoring.

```bash
clawhip tmux new -s issue-2000 \
  --channel 1468539002985644084 \
  --mention '<@botid>' \
  --keywords 'error,PR created,FAILED,complete' \
  --stale-minutes 10 \
  --format alert \
  -- cargo test
```

Wrapper-specific arguments are parsed **before** `--`:

- `--channel <id>`
- `--mention <tag>`
- `--keywords <comma-separated>`
- `--stale-minutes <n>`
- `--format <compact|alert|inline>`
- `-s, --session <name>`
- `-n, --window-name <name>`
- `-c, --cwd <dir>`
- `--attach`

Everything after `--` is passed through to the command running inside tmux.

## Webhook API

The daemon exposes:

- `GET /health`
- `GET /api/status`
- `POST /api/event`
- `POST /events`
- `POST /api/tmux/register`
- `POST /github`

### GitHub webhook support

`POST /github` supports:
- `issues.opened` -> `github.issue-opened`
- `pull_request.opened` -> `git.pr-status-changed`
- `pull_request.reopened` -> `git.pr-status-changed`
- `pull_request.closed` -> `git.pr-status-changed` with `closed` or `merged`

## Config

Config file:

```text
~/.clawhip/config.toml
```

Example:

```toml
[discord]
bot_token = "your-discord-bot-token"

[daemon]
bind_host = "0.0.0.0"
port = 25294
base_url = "http://127.0.0.1:25294"

[defaults]
channel = "1468539002985644084"
format = "compact"

[[routes]]
event = "github.*"
filter = { repo = "oh-my-claudecode" }
channel = "1468539002985644084"
format = "compact"

[[routes]]
event = "github.*"
filter = { repo = "clawhip" }
channel = "9999999999"
format = "alert"

[[routes]]
event = "tmux.*"
filter = { session = "issue-*" }
channel = "1468539002985644084"
format = "compact"

[monitors]
poll_interval_secs = 5
github_api_base = "https://api.github.com"
# github_token = "optional-token"

[[monitors.git.repos]]
path = "/home/user/Workspace/clawhip"
name = "clawhip"
remote = "origin"
emit_commits = true
emit_branch_changes = true
emit_pr_status = true
channel = "1468539002985644084"
format = "compact"

[[monitors.tmux.sessions]]
session = "issue-*"
keywords = ["error", "FAILED", "complete", "PR created"]
stale_minutes = 10
channel = "1468539002985644084"
mention = "<@botid>"
format = "alert"
```

## Route filtering

Routes support payload-based filters so the same event type can go to different channels.

```toml
[[routes]]
event = "github.*"
filter = { repo = "oh-my-claudecode" }
channel = "1468539002985644084"
format = "compact"

[[routes]]
event = "github.*"
filter = { repo = "clawhip" }
channel = "9999999999"
format = "alert"

[[routes]]
event = "tmux.*"
filter = { session = "issue-*" }
channel = "1468539002985644084"
```

Filter values support glob matching.

## Config commands

```bash
clawhip config
clawhip config show
clawhip config path
```

## systemd deployment

A ready-to-use unit file is included at:

```text
deploy/clawhip.service
```

Typical install flow:

```bash
sudo cp deploy/clawhip.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now clawhip
sudo systemctl status clawhip
```

## Environment variables

- `CLAWHIP_CONFIG`
- `CLAWHIP_DAEMON_URL`
- `CLAWHIP_DISCORD_BOT_TOKEN`
- `CLAWHIP_DISCORD_API_BASE`
- `CLAWHIP_GITHUB_TOKEN`
- `CLAWHIP_GIT_BIN`
- `CLAWHIP_TMUX_BIN`

## Development

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build
```
