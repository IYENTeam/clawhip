# Context Snapshot — clawhip event enhancement based on agentika

- task statement: Compare ../agentika and clawhip, then produce a ralplan for clawhip event enhancements inspired by agentika.
- desired outcome: A grounded comparison plus an approved enhancement plan that fits clawhip's daemon-first Discord routing model without turning it into a general-purpose broker.
- known facts/evidence:
  - clawhip already has typed incoming event normalization, canonical session/agent event mapping, route filters, multi-delivery routing, renderer/sink separation, and git/GitHub/tmux monitors.
  - clawhip routing is currently best-effort async delivery over an in-memory Tokio mpsc queue with no durable event log, replay, ack/nack, or DLQ.
  - agentika is an append-only event broker with EventEnvelope metadata, topic offsets, current-state snapshots, schema controls, SSE tailing, consumer groups, dedup/idempotency, OCC, delayed delivery, DLQ, and artifacts.
  - likely relevant clawhip touchpoints: src/events.rs, src/router.rs, src/dispatch.rs, src/config.rs, docs/native-event-contract.md, ARCHITECTURE.md.
  - likely relevant agentika touchpoints: docs/event-system.md, src/model/event.rs, src/model/metadata.rs, src/server/handlers.rs, src/core/*.
- constraints:
  - clawhip must stay lightweight in the daemon hot path.
  - config changes must remain backward-compatible.
  - no new dependencies unless justified.
  - enhancement should focus on event-system improvements that materially help Discord/Slack routing, observability, and recovery.
- unknowns/open questions:
  - which agentika patterns give highest value to clawhip without importing broker complexity.
  - whether enhancements should be runtime-only, CLI-visible, or config-surface visible.
  - what minimal verification path would prove the enhancement.
- likely codebase touchpoints:
  - src/events.rs
  - src/router.rs
  - src/dispatch.rs
  - src/config.rs
  - docs/native-event-contract.md
  - ARCHITECTURE.md

## Scope update
- user clarified bidirectional goal: transfer clawhip strengths into agentika too, not only agentika strengths into clawhip.
- clawhip -> agentika candidates: canonical native-event normalization, route/filter grammar, renderer/sink split, operational mention/template policy, source adapters for git/GitHub/tmux/session workflows, CLI ergonomics for emitting operational events.
- agentika -> clawhip candidates: durable event envelope, correlation metadata, replay/tail, delayed delivery for retries, failure capture/DLQ, lightweight journaling, stronger delivery observability.
