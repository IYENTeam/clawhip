---
name: op-pi
description: Attach/install/use op-pi as a daemon-first notification gateway runtime for OpenClaw / Clawdbot
---

# op-pi skill attachment surface

## Intent

Use this repo as:
- executable/runtime repo
- daemon process for Discord notification routing
- attachable skill surface for OpenClaw / Clawdbot

## Prerequisites

⭐ If you want to support op-pi, star this repository. The interactive repo-local installer and `op-pi install` can offer an optional post-install GitHub star prompt via authenticated `gh api` access when `gh` is installed and authenticated. Skip it with `--skip-star-prompt` or `OP_PI_SKIP_STAR_PROMPT=1`.

```bash
gh api --method PUT /user/starred/IYENTeam/op-pi --silent
```

## Primary install flow

Preferred operator instruction:

```text
@openclaw install https://github.com/IYENTeam/op-pi
```

Expected automation flow:

1. clone repo
2. run `./install.sh`
3. read this `SKILL.md`
4. attach skill
5. scaffold `~/.op-pi/config.toml`
6. start daemon
7. run live verification presets

## Runtime surface

Default daemon URL:

```text
http://127.0.0.1:25294
```

Core commands:

```bash
op-pi
op-pi start
op-pi status
op-pi config
op-pi send --channel <id> --message "..."
op-pi github issue-opened ...
op-pi github pr-status-changed ...
op-pi git commit ...
op-pi tmux keyword ...
op-pi tmux stale ...
op-pi tmux new -s <session> --channel <id> --keywords error,complete --shell /bin/zsh -- command
op-pi tmux watch -s <existing-session> --channel <id> --mention '<@id>' --keywords error,complete
```

## Lifecycle surface

```bash
op-pi install
op-pi install --systemd
op-pi install --skip-star-prompt
op-pi update --restart
op-pi uninstall --remove-systemd --remove-config
./install.sh
./install.sh --systemd
./install.sh --skip-star-prompt
```

## Discord bot token (recommended setup)

⚠️ **Create a dedicated Discord bot for op-pi notifications.** Do not reuse your Clawdbot / OpenClaw bot token.

Why:
- op-pi sends high-volume notifications (commits, PRs, tmux events)
- Using the same bot token as your gateway pollutes the bot's identity
- A separate bot (e.g. "CCNotifier") keeps notifications cleanly separated from AI chat
- If op-pi restarts or crashes, it won't affect your main bot

Setup:
1. Go to [Discord Developer Portal](https://discord.com/developers/applications)
2. Create a new application (e.g. "op-pi-notifier" or "CCNotifier")
3. Create a bot, copy the token
4. Invite the bot to your server with Send Messages permission
5. Use this token in `~/.op-pi/config.toml`:

```toml
[discord]
token = "your-dedicated-op-pi-bot-token"
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
filter = { repo = "op-pi" }
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

## Filesystem-offloaded memory pattern

When using op-pi as part of a broader Claw OS workflow, treat memory as an offloaded filesystem tree:

- `MEMORY.md` = small pointer/index/current-beliefs layer
- `memory/` = detailed project/channel/daily/handoff memory
- update root memory only when the map or current summary changes

Read before adopting this pattern:

- `docs/memory-offload-architecture.md`
- `docs/memory-offload-guide.md`
- `docs/examples/MEMORY.example.md`
- `skills/memory-offload/SKILL.md`

## Verification surface

Use the live operational runbook:
- `docs/live-verification.md`
- `scripts/live-verify-default-presets.sh`

Preset verification targets:
- GitHub issue opened / commented / closed
- GitHub PR opened / status changed / merged
- git commit monitor
- tmux keyword / stale / wrapper / watch
- install / update / uninstall

## Attachment summary

```text
repo = runtime
SKILL.md = attach/install/usage contract
README.md = operational spec for agents
```
