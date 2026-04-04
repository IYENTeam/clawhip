# clawhip — AGENTS.md

Daemon-first event gateway for Discord. Routes GitHub, tmux, and custom events to channels.

## Working agreements

- Rust: follow existing code style, use `anyhow` for error handling.
- Config changes must be backward-compatible (old configs must still parse).
- New routes/filters: add integration tests.
- CLI subcommands: include `--help` descriptions for all flags.
- Keep the daemon lightweight — no unnecessary allocations in the hot path.

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

<!-- OMX:RUNTIME:START -->
<session_context>
**Session:** omx-1775288262135-xflyj9 | 2026-04-04T07:37:42.500Z

**Codebase Map:**
  integrations/: clawhip-hook, clawhip-sdk

**Explore Command Preference:** enabled via `USE_OMX_EXPLORE_CMD` (default-on; opt out with `0`, `false`, `no`, or `off`)
- Advisory steering only: agents SHOULD treat `omx explore` as the default first stop for direct inspection and SHOULD reserve `omx sparkshell` for qualifying read-only shell-native tasks.
- For simple file/symbol lookups, use `omx explore` FIRST before attempting full code analysis.
- When the user asks for a simple read-only exploration task (file/symbol/pattern/relationship lookup), strongly prefer `omx explore` as the default surface.
- Explore examples: `omx explore...

**Compaction Protocol:**
Before context compaction, preserve critical state:
1. Write progress checkpoint via state_write MCP tool
2. Save key decisions to notepad via notepad_write_working
3. If context is >80% full, proactively checkpoint state
</session_context>
<!-- OMX:RUNTIME:END -->
