---
name: clawhip
description: Configure and manage clawhip — the daemon-first event gateway for Discord
---

# clawhip

`clawhip` is a daemon-first notification gateway.

## Essentials

- daemon default port: `25294`
- start daemon: `clawhip` or `clawhip start`
- check health: `clawhip status`
- send event through daemon: `clawhip send --channel <id> --message "..."`
- wrapper mode: `clawhip tmux new -s <session> --channel <id> --keywords error,complete -- command`

## Config

Config file: `~/.clawhip/config.toml`

Key sections:
- `[discord]`
- `[daemon]`
- `[[routes]]`
- `[[monitors.git.repos]]`
- `[[monitors.tmux.sessions]]`

## Architecture

```text
[CLI clients / webhooks / daemon monitors] -> [clawhip daemon :25294] -> [route filters] -> [Discord REST API]
```
