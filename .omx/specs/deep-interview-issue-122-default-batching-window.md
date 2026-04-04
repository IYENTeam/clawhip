# Deep Interview Spec — issue #122 default batching window

## Metadata
- profile: quick
- rounds: 1
- final ambiguity: 0.16
- threshold: 0.30
- context type: brownfield
- context snapshot: `.omx/context/issue-122-default-batching-window-20260403T010047Z.md`

## Intent
Reduce noisy burst delivery and overly eager mentions without delaying urgent/failure notifications.

## Desired Outcome
A configurable dispatcher-level routine batching window with a default around 5 seconds, plus tests and documentation for grouped delivery behavior.

## In Scope
- Minimal dispatch config surface for a routine batching window.
- Dispatcher-level routine burst batching in the narrowest safe place.
- Explicit urgent/failure bypass behavior.
- Grouped delivery tests.
- PR/documentation notes explaining batching semantics and mention impact.

## Out of Scope / Non-goals
- A per-route batching policy system.
- Reworking every source-specific debounce/window mechanism.
- A new durable queue or scheduler layer.
- Broad sink-agnostic batching redesign unless the dispatcher-level path proves unsafe.

## Decision Boundaries
- OMX may choose the exact config field name and dispatcher insertion point.
- OMX may define a conservative urgent-bypass rule for alert/failure paths.
- OMX should not add multiple knobs when one backward-compatible dispatch setting is sufficient.

## Constraints
- Backward-compatible config parsing.
- Default remains around 5 seconds.
- English-only code/comments/PR text.
- Scope stays tight and mergeable.

## Testable Acceptance Criteria
1. Default config enables routine batching with an approximately 5-second dispatch window.
2. Users can override the routine batching window cleanly through config.
3. Routine burst deliveries group into a single outbound send in targeted tests.
4. Urgent/failure paths identified for v1 bypass the routine batching delay.
5. Mention behavior is less eager for grouped routine bursts and this behavior is documented.
