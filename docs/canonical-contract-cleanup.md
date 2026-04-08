# Canonical contract cleanup note

This historical audit has been superseded by the provider-native Codex + Claude hook rewrite.

Current source-of-truth docs:

- `docs/native-event-contract.md` — routing, metadata, and additive augmentation guidance
- `docs/event-contract-v1.md` — frozen shared-event contract
- `docs/live-verification.md` — verification workflow and regression evidence expectations

When updating this area, prefer documenting the shared provider-native surface rather than older
wrapper- or bridge-specific integration paths.

Effect:

- `tmux.keyword` and `git.commit` no longer collapse into the same routine batch just because they share a Discord target
- batch content stays semantically coherent
- this remains backward-compatible for existing single-family burst batching

## Initial canonical contract rules

For incremental adoption, route authors should prefer this vocabulary:

- `event` / `contract_event`
- `tool`
- `repo_name`
- `session_name`
- `issue_number`
- `pr_number`
- `branch`
- `channel_hint`

Legacy aliases remain supported:

- `repo`
- `session`
- `channel`

## Follow-up slices

### Slice A: source payload parity

Optionally teach git / GitHub / tmux sources to emit the canonical names directly in payloads (`repo_name`, `session_name`) in addition to legacy keys.

### Slice B: explicit delivery contract type

Promote a documented delivery identity object around:

- canonical event kind
- sink
- target
- format
- mention
- template
- dynamic-token policy

### Slice C: renderer contract fixtures

Add snapshot-style tests for one canonical example per event family:

- `session.*`
- `workspace.*`
- `git.*`
- `github.*`
- `tmux.*`
- batched CI / routine summaries

## Files tied to this pass

- `src/events.rs`
- `src/router.rs`
- `src/dispatch.rs`
- `docs/canonical-contract-cleanup.md`
