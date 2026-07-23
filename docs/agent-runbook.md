# op_pi Agent Runbook

## Build

```bash
cargo build --release
```

Binary: `target/release/op_pi`

## Test

```bash
cargo test          # 453+ tests, must all pass
cargo fmt --check   # format check
cargo clippy        # lint check — fix ALL warnings before push
```

**CI runs fmt + clippy + test. Fix ALL three locally before push.** (2026-05-18 incident: pushed fmt-only fix, clippy failed → CI re-failed.)

## Deploy

```bash
# Build on Mac mini (SSOT)
cargo build --release

# Copy to wherever needed
cp target/release/op_pi /usr/local/bin/op_pi

# Restart
# (systemd on VMs, or direct process on Mac mini)
```

## Config

- Config file: `~/.op_pi/config.toml`
- State file: configured via `github_monitor_state_path` (MUST be set, otherwise CI flood on restart)
- Repo metadata: `~/.op_pi/github/` (CI watchdog metadata ONLY, no source code)
- Source code: `~/projects/` (never under .op_pi)

## Common Operations

### Add repo monitoring
```bash
op_pi repo add <owner/repo>
```

### Check monitoring status
```bash
op_pi watch status
```

### Manual event send
```bash
op_pi send --channel <ID> --message "text"
```

## Incident Patterns

### CI event flood on restart (2026-05-19)
- Symptom: dozens of stale CI events sent on op_pi restart
- Cause: no persistent state + no date filter + cold start treated all runs as new
- Fix: state persistence + 7-day filter + cold start skip
- Prevention: always set `github_monitor_state_path` in config

## Forbidden Actions

- `git push --force` on main
- Cloning source code under `~/.op_pi/github/` (metadata only)
- Pushing without `cargo fmt + clippy + test` all passing
