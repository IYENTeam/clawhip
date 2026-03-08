---
name: clawhip
description: Attach/install/use clawhip as a daemon-first notification gateway runtime for OpenClaw / Clawdbot
---

# clawhip skill attachment surface

## Intent

Use this repo as:
- executable/runtime repo
- daemon process for Discord notification routing
- attachable skill surface for OpenClaw / Clawdbot

## Primary install flow

Preferred operator instruction:

```text
@openclaw install https://github.com/Yeachan-Heo/clawhip
```

Expected automation flow:

1. clone repo
2. run `./install.sh`
3. read this `SKILL.md`
4. attach skill
5. scaffold `~/.clawhip/config.toml`
6. start daemon
7. run live verification presets

## Runtime surface

Default daemon URL:

```text
http://127.0.0.1:25294
```

Core commands:

```bash
clawhip
clawhip start
clawhip status
clawhip config
clawhip send --channel <id> --message "..."
clawhip github issue-opened ...
clawhip github pr-status-changed ...
clawhip git commit ...
clawhip tmux keyword ...
clawhip tmux stale ...
clawhip tmux new -s <session> --channel <id> --keywords error,complete --shell /bin/zsh -- command
```

## Lifecycle surface

```bash
clawhip install
clawhip install --systemd
clawhip update --restart
clawhip uninstall --remove-systemd --remove-config
./install.sh
./install.sh --systemd
```

## Config scaffold expectations

Key sections:
- `[discord]`
- `[daemon]`
- `[defaults]`
- `[[routes]]`
- `[monitors]`
- `[[monitors.git.repos]]`
- `[[monitors.tmux.sessions]]`

Typical preset route:

```toml
[[routes]]
event = "github.*"
filter = { repo = "clawhip" }
channel = "1480171113253175356"
mention = "<@1465264645320474637>"
format = "compact"
```

## Dynamic template opt-in

```toml
[[routes]]
event = "tmux.*"
allow_dynamic_tokens = true
template = "{session}\n{tmux_tail:issue-1456:20}\n{iso_time}"
```

Allowed dynamic tokens:
- `{sh:...}`
- `{tmux_tail:session:lines}`
- `{file_tail:/path:lines}`
- `{env:NAME}`
- `{now}`
- `{iso_time}`

## Verification surface

Use the live operational runbook:
- `docs/live-verification.md`
- `scripts/live-verify-default-presets.sh`

Preset verification targets:
- GitHub issue opened / commented / closed
- GitHub PR opened / status changed / merged
- git commit monitor
- tmux keyword / stale / wrapper
- install / update / uninstall

## Attachment summary

```text
repo = runtime
SKILL.md = attach/install/usage contract
README.md = operational spec for agents
```
