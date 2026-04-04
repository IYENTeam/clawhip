# Changelog

## 0.5.3 - 2026-04-04

### Highlights

- fix `clawhip send --channel` being overridden by route or default channel config
- for `custom` events, the explicit event channel now takes highest priority over route and default channels

### Upgrade notes

- crate version is now `0.5.3`
- existing config remains compatible; no migration required
- if you relied on a catch-all `event = "custom"` route to redirect all `clawhip send` traffic to a specific channel, that route channel will now only apply when `--channel` is not specified

## 0.5.2 - 2026-04-04

### Highlights

- reduced routine Discord burst noise with configurable batching for routine notifications
- allow `stale_minutes = 0` to disable tmux stale detection cleanly
- keep cron startup alive when persisted scheduler state is empty or invalid
- surface source failures as degraded alerts before the daemon appears healthy
- make matched route channels override source-provided channel hints consistently
- quiet invalid git monitor paths so they stop drowning out actionable failures

### Upgrade notes

- crate version is now `0.5.2`
- existing config remains compatible; no schema migration is required for this patch release
- `stale_minutes = 0` is now treated as an explicit disable for tmux stale alerts

## 0.3.0 - 2026-03-09

### Highlights

- introduced the typed internal event model used by the dispatcher pipeline
- generalized routing so one event can fan out to multiple deliveries
- extracted git, GitHub, and tmux monitoring into explicit event sources
- split rendering from transport and shipped the Discord sink on top of that boundary
- kept existing CLI and HTTP event ingress compatible while normalizing into the new architecture

### Upgrade notes

- crate version is now `0.3.0`
- `[providers.discord]` is the preferred config surface; legacy `[discord]` remains compatible
- routes may set `sink = "discord"`; omitting it still defaults to Discord in this release
