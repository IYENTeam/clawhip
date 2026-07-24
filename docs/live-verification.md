# Live verification workflow for built-in presets

This document is for **real operational verification**, not mock-only tests.

## Preconditions

- running `op_pi` daemon
- real Discord bot token with access to the test channel
- real GitHub auth (`gh auth status` should succeed)
- tmux installed locally
- route filters configured for the target repo/session/channel

Recommended environment:

```bash
export OP_PI_REPO=IYENTeam/op_pi
export OP_PI_CHANNEL=TEST_CHANNEL_ID
export OP_PI_DAEMON_URL=http://127.0.0.1:25294
export OP_PI_BOT_TOKEN='<discord-bot-token>'
export OP_PI_MENTION='@maintainer-or-team'
```

## Real built-in preset checklist

### GitHub issue presets

- issue opened
- issue commented
- issue closed

Operational flow:

1. Create a real issue in the target repo.
2. Wait for daemon monitor pickup or webhook delivery.
3. Confirm a real Discord message arrives in the configured test channel.
4. Add a real comment to the issue.
5. Confirm the issue-commented message arrives.
6. Close the issue.
7. Confirm the issue-closed message arrives.

### GitHub PR presets

- PR opened
- PR status changed
- PR merged

Operational flow:

1. Create a temporary base branch and feature branch.
2. Push the feature branch.
3. Open a real PR against the temporary base branch.
4. Confirm the PR-opened / status-changed message arrives.
5. Merge the temporary PR.
6. Confirm the merged status message arrives.
7. Delete temporary branches if desired.

### Provider-native Codex + Claude contract

- shared event set: `SessionStart`, `PreToolUse`, `PostToolUse`, `UserPromptSubmit`, `Stop`
- generic ingestion via `op_pi native hook --provider <codex|claude>`

Operational flow:

1. Enable provider-native hooks in a real Codex or Claude Code workspace:
   - Codex: `op_pi hooks install --provider codex --scope global` or `--scope project` (matching the official Codex `hooks.json` search locations)
   - Claude Code: `op_pi hooks install --provider claude-code --scope global`
2. Pipe one representative Codex payload through the generic native ingress:

```bash
printf '%s\n' '{
  "session_id": "sess-65",
  "cwd": "/repo/op_pi",
  "event": "SessionStart"
}' | op_pi native hook --provider codex
```

3. Confirm op_pi accepts it and renders a stable lifecycle message with project/repo context.
4. Repeat with a representative Claude payload:

```bash
printf '%s\n' '{
  "session_id": "sess-65",
  "cwd": "/repo/op_pi",
  "event": "SessionStart"
}' | op_pi native hook --provider claude
```

5. Confirm both providers normalize into the same shared route family.
6. Send representative payloads for `PreToolUse`, `PostToolUse`, `UserPromptSubmit`, and `Stop`.
7. Confirm additive augmentation still preserves the base routing keys when `.op_pi/hooks/` is enabled.

### tmux presets

- keyword detection
- stale detection
- tmux wrapper registration path

Operational flow:

1. Launch a real Codex or Claude session with provider-native hooks enabled.
2. Verify the pane is actually alive before trusting any `agent.started` message.
3. Confirm routed delivery in Discord.
4. Print a configured keyword (`error`, `FAILED`, `PR created`, etc) only when intentionally testing keyword behavior.
5. Leave the session idle beyond the stale threshold only when intentionally testing stale behavior.
6. Inspect `op_pi tmux list` to confirm exactly which watch registrations exist.
7. If alert text disagrees with pane reality, treat it as monitor noise and debug registration overlap / stale math before assuming session failure.

## Helper script

A helper script is included:

```bash
scripts/live_verify_default_presets.sh <mode>
```

Available modes:

- `issue-opened`
- `issue-comment`
- `issue-closed`
- `pr-opened`
- `pr-merged`
- `tmux-keyword`
- `tmux-stale`
- `tmux-wrapper`

The script is intentionally conservative: it prints the live workflow and fetches recent Discord messages, but it does not silently mutate production resources without operator intent.

## Verified live run already completed

On March 8, 2026, a real validation was run for the GitHub issue-opened monitor path:

- real issue created on `IYENTeam/op_pi`
- daemon monitor emitted `github.issue-opened`
- real Discord delivery observed with route-level mention prepended
- issue closed after verification

On March 11, 2026, a real validation was run for the custom send path:

- local daemon health/status returned ok on `http://127.0.0.1:25294`
- `cargo run -q -- send --message "🧪 op_pi live verification (...)"` exited successfully
- guild-wide search confirmed actual Discord delivery by the `op_pi` webhook bot
- delivery landed in the configured test channel, confirming the configured wildcard webhook route was active

On July 24, 2026, a real Victor Google Calendar validation was run through the
production `https://op-pi.iyendev.com/google/calendar` callback:

- Victor created `[op_pi QA hardened] Calendar sync 20260724-qa2` in the Google
  Calendar web UI; the deployed macmini emitted `calendar.event.created`
- Victor renamed it to `[op_pi QA hardened] Calendar sync FINAL
  20260724-qa2` and added a Google Meet conference; op_pi emitted
  `calendar.event.updated` with the final title, time range, and Meet URL
- Victor deleted the event in the web UI; op_pi emitted
  `calendar.event.cancelled` with the same final title, time, and Meet URL
- a Calendar API `showDeleted=false` query returned zero active op_pi QA
  events after cleanup
- deployed Calendar health reported `status=running`, cursor present,
  `outbox_depth=0`, no pending/retiring channels, and null sync/watch errors

This run used the real Victor account, Google watch channel, Cloudflare public
route, op_pi daemon, incremental Calendar API query, typed renderer, and
configured localfile destination. It was not a synthetic webhook-only test.
