# Changelog

## 0.5.1 - 2026-04-02

### Highlights

- made native OMX hook envelopes a first-class clawhip integration surface
- added tmux watch audit trail and active watch listing
- made the CI batch window configurable
- fixed route-channel handling for tmux session startup events

### Upgrade notes

- crate version is now `0.5.1`
- native OMX hook-bridge + SDK setup (`integrations/omx/`, `clawhip omx hook`, `/api/omx/hook`) is the default/recommended integration path
- no config migration is required for this patch release

## 0.4.0 - 2026-03-11

### Highlights

- added clone-local install lifecycle polish: repo-local `install.sh`, `clawhip install`, and `clawhip update`/`uninstall` now cover the current dev build workflow more cleanly
- added an optional post-install GitHub star prompt for interactive installs, with explicit opt-in only and skip controls for both the shell installer and CLI install path
- shipped `clawhip memory init` and `clawhip memory status` for filesystem-offloaded memory scaffolds in repos and workspaces
- normalized native OMC/OMX payloads into the lower-noise `session.*` contract while keeping legacy `agent.*` compatibility
- refreshed live verification guidance around daemon health/status and custom send delivery

### Upgrade notes

- crate version is now `0.4.0`
- interactive install flows may offer a GitHub star prompt only when `gh` is installed and authenticated; disable it with `--skip-star-prompt` or `CLAWHIP_SKIP_STAR_PROMPT=1`
- runtime memory scaffolds can now be bootstrapped and inspected with `clawhip memory init` and `clawhip memory status`
- existing config remains compatible; no config migration is required for this release

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
