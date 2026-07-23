# op-pi — AGENTS.md

Local-first operation pipeline for typed events. Routes development, agent, and
cloud signals to Discord, Slack, and local interfaces.

## Working agreements

- Rust: follow existing code style, use `anyhow` for error handling.
- Config changes must be backward-compatible (old configs must still parse).
- New routes/filters: add integration tests.
- CLI subcommands: include `--help` descriptions for all flags.
- Keep the daemon lightweight — no unnecessary allocations in the hot path.
- All CI checks must pass locally before push: `cargo fmt --check && cargo clippy && cargo test`
- Never push fmt-only fixes — always run clippy too (2026-05-18 incident).

## Build & Test

```bash
cargo build --release   # build
cargo test              # 453+ tests
cargo fmt --check       # format
cargo clippy            # lint — ALL warnings must be fixed
```

## Key Paths

- `src/main.rs` — entry point
- `src/source/github.rs` — GitHub polling + event generation
- `src/discord.rs` — Discord message sending
- `~/.op-pi/config.toml` — runtime config
- `~/.op-pi/github/` — CI watchdog metadata ONLY (no source code)

## Forbidden

- Never clone source code under `~/.op-pi/github/` (metadata only)
- Never push without all 3 checks passing (fmt + clippy + test)
- Never hardcode tokens or secrets
- Never send to Discord channels not in config

## Review guidelines

- Flag breaking changes to config schema or CLI interface as P0.
- Verify all Discord API calls handle rate limits and error responses.
- Check for hardcoded tokens or secrets — flag as P0.
- TOML config parsing: ensure new fields have sensible defaults (don't break existing configs).
- Route matching: verify glob patterns are tested with edge cases.
- tmux integration: confirm session monitoring handles missing/dead sessions gracefully.
- New dependencies must be justified — prefer std library where possible.
- Daemon lifecycle: verify clean shutdown and signal handling.
- Test coverage: flag new logic paths that lack corresponding tests.
