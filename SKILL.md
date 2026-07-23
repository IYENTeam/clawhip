---
name: op_pi
description: Attach/install/use op_pi as a daemon-first notification gateway runtime for OpenClaw / Clawdbot
---

# op_pi skill attachment surface

## Intent

Use this repo as:
- executable/runtime repo
- daemon process for Discord notification routing
- attachable skill surface for OpenClaw / Clawdbot

## Prerequisites

⭐ If you want to support op_pi, star this repository. The interactive repo-local installer and `op_pi install` can offer an optional post-install GitHub star prompt via authenticated `gh api` access when `gh` is installed and authenticated. Skip it with `--skip-star-prompt` or `OP_PI_SKIP_STAR_PROMPT=1`.

```bash
gh api --method PUT /user/starred/IYENTeam/op_pi --silent
```

## Primary install flow

Preferred operator instruction:

```text
@openclaw install https://github.com/IYENTeam/op_pi
```

Expected automation flow:

1. clone repo
2. run `./install.sh`
3. read this `SKILL.md`
4. attach skill
5. scaffold `~/.op_pi/config.toml`
6. start daemon
7. run live verification presets

## Runtime surface

Default daemon URL:

```text
http://127.0.0.1:25294
```

Core commands:

```bash
op_pi
op_pi start
op_pi status
op_pi config
op_pi send --channel <id> --message "..."
op_pi github issue-opened ...
op_pi github pr-status-changed ...
op_pi git commit ...
op_pi tmux keyword ...
op_pi tmux stale ...
op_pi tmux new -s <session> --channel <id> --keywords error,complete --shell /bin/zsh -- command
op_pi tmux watch -s <existing-session> --channel <id> --mention '<@id>' --keywords error,complete
```

## Lifecycle surface

```bash
op_pi install
op_pi install --systemd
op_pi install --skip-star-prompt
op_pi update --restart
op_pi uninstall --remove-systemd --remove-config
./install.sh
./install.sh --systemd
./install.sh --skip-star-prompt
```

## Discord bot token (recommended setup)

⚠️ **Create a dedicated Discord bot for op_pi notifications.** Do not reuse your Clawdbot / OpenClaw bot token.

Why:
- op_pi sends high-volume notifications (commits, PRs, tmux events)
- Using the same bot token as your gateway pollutes the bot's identity
- A separate bot (e.g. "CCNotifier") keeps notifications cleanly separated from AI chat
- If op_pi restarts or crashes, it won't affect your main bot

Setup:
1. Go to [Discord Developer Portal](https://discord.com/developers/applications)
2. Create a new application (e.g. "op_pi-notifier" or "CCNotifier")
3. Create a bot, copy the token
4. Invite the bot to your server with Send Messages permission
5. Use this token in `~/.op_pi/config.toml`:

```toml
[providers.discord]
bot_token = "your-dedicated-op_pi-bot-token"
```

## Config scaffold expectations

Key sections:
- `[providers.discord]`
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
filter = { repo = "op_pi" }
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

When using op_pi as part of a broader Claw OS workflow, treat memory as an offloaded filesystem tree:

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
- `scripts/live_verify_default_presets.sh`

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
