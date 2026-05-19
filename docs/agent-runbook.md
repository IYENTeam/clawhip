# clawhip Agent Runbook

## Build

```bash
cargo build --release
```

Binary: `target/release/clawhip`

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
cp target/release/clawhip /usr/local/bin/clawhip

# Restart
# (systemd on VMs, or direct process on Mac mini)
```

## Config

- Config file: `~/.clawhip/config.toml`
- State file: configured via `github_monitor_state_path` (MUST be set, otherwise CI flood on restart)
- Repo metadata: `~/.clawhip/github/` (CI watchdog metadata ONLY, no source code)
- Source code: `~/projects/` (never under .clawhip)

## Common Operations

### Add repo monitoring
```bash
clawhip repo add <owner/repo>
```

### Check monitoring status
```bash
clawhip watch status
```

### Manual event send
```bash
clawhip send --channel <ID> --message "text"
```

## Incident Patterns

### CI event flood on restart (2026-05-19)
- Symptom: dozens of stale CI events sent on clawhip restart
- Cause: no persistent state + no date filter + cold start treated all runs as new
- Fix: state persistence + 7-day filter + cold start skip
- Prevention: always set `github_monitor_state_path` in config

## Forbidden Actions

- `git push --force` on main
- Cloning source code under `~/.clawhip/github/` (metadata only)
- Pushing without `cargo fmt + clippy + test` all passing
