# PRD — issue #122 configurable default batching window

## Requirements Summary
Add a minimal first-slice batching mechanism for routine burst notifications so clawhip stops sending overly granular immediate Discord notifications by default. Keep the batching window configurable, default it to 5 seconds, allow `0` to disable it, and document which urgent/failure paths bypass batching.

## Brownfield Evidence
- The dispatcher is the narrowest shared send path: `src/dispatch.rs:17-155`.
- clawhip already uses dispatcher-managed CI batching with a timer wheel in `src/dispatch.rs:165-409`, so batching at this layer matches existing architecture.
- The daemon already feeds dispatch config into the dispatcher constructor in `src/daemon.rs:59-62`.
- Config parsing/default/validation currently lives in `src/config.rs:86-103` and `src/config.rs:573-576`.
- Mentions are prepended in router rendering at `src/router.rs:119-145`, so grouped routine delivery needs an explicit mention policy instead of blindly concatenating already-mentioned messages.
- Source-specific windows exist for tmux/workspace (`src/source/tmux.rs:641-697`, `src/source/workspace.rs:245-299`) but do not solve cross-source routine delivery bursts.

## RALPLAN-DR Summary
### Principles
1. Batch at the narrowest shared delivery seam, not per-source.
2. Keep v1 to one new dispatch knob with a 5-second default.
3. Do not delay urgent/failure traffic or double-batch existing CI flows.
4. Make grouped routine bursts quieter by reducing mention eagerness.

### Decision Drivers
1. Reduce Discord send churn / gateway noise for routine bursts.
2. Preserve timeliness for alerts, failures, blocked/stale states.
3. Keep the slice mergeable: one config field, one dispatcher seam, targeted tests.

### Viable Options
#### Option A — Delivery-level batching after `router.resolve()`, before `router.render_delivery()` / `sink.send()`
- Pros:
  - Uses the existing shared outbound choke point in `src/dispatch.rs:103-155`.
  - Route target, format, and mention policy are known before content is finalized.
  - Avoids touching every source.
- Cons:
  - Needs a small queued-delivery type and grouped rendering path.

#### Option B — Raw event batching before route resolution
- Pros:
  - Conceptually simple.
- Cons:
  - Unsafe for fan-out because one `IncomingEvent` can resolve to multiple routes, sinks, and mentions.
  - Risks wrong grouping and wrong mention behavior.

#### Option C — Discord sink coalescing after render
- Pros:
  - Closest to the Discord boundary.
- Cons:
  - Too late for route/mention decisions because rendered content already contains mention prefixes (`src/router.rs:141-143`).
  - Harder to keep future non-Discord behavior coherent.

### Recommended Decision
Choose Option A: batch resolved routine Discord deliveries after `router.resolve()` and before `router.render_delivery()` / `sink.send()`, controlled by a single new dispatch config field: `dispatch.routine_batch_window_secs`.

## ADR
- Decision: Implement a delivery-level routine batching window with `dispatch.routine_batch_window_secs = 5` by default and `0` meaning disabled.
- Drivers:
  - Dispatcher is the narrowest shared delivery point.
  - Existing CI batching proves the timer/tick model already works here.
  - Owner explicitly requires configurability without a large config system.
- Alternatives considered:
  - Source-specific batching: rejected for broader scope and inconsistent behavior.
  - Raw event batching before route resolution: rejected because route fan-out makes safe grouping too ambiguous.
  - Sink-level coalescing: rejected because mention/render decisions are already baked into rendered content.
- Why chosen:
  - Best scope-to-impact ratio for reducing routine burst noise.
  - Keeps urgent/failure bypass logic centralized.
  - Allows a clean config knob without adding route-level policy complexity.
- Consequences:
  - `DispatchConfig` grows one new field for routine batching.
  - `Dispatcher` gains a routine-delivery batcher keyed by a delivery signature: sink + target + format + normalized mention + template + allow_dynamic_tokens.
  - Dispatch/router code needs a small seam so grouped batches can render bodies and apply grouped mention policy at flush time.
- Follow-ups:
  - Consider per-route opt-outs only if real workloads need them.
  - Reassess Slack applicability after the Discord-first slice lands.

## Batching Semantics
- New config field: `dispatch.routine_batch_window_secs`
  - default: `5`
  - override: any positive integer seconds
  - disable: `0`
- Scope for v1: routine Discord deliveries only.
- Batching key for v1: an explicit delivery signature: `sink + target + format + normalized mention + template + allow_dynamic_tokens`.
- Insertion point: batch resolved deliveries after `router.resolve()` and before `router.render_delivery()` / `sink.send()`.
- Existing `github.ci-*` event handling keeps its current CI-specific batching path and bypasses the new routine batcher.

## Urgent / Failure Bypass Policy
The first slice should send immediately when any of the following are true:
1. event kind ends with `.failed`
2. event kind ends with `.blocked`
3. event kind is `tmux.stale`
4. event kind matches `github.ci-*` (keep existing CI batching only)

## Mention Behavior
- For grouped routine batches with 2+ items: suppress the mention entirely.
- For a queued routine batch that flushes with 1 item: preserve the existing mention behavior.
- PR notes and code comments should call out that the quieter mention rule applies only to grouped routine bursts.

## Acceptance Criteria
1. `AppConfig::default()` enables routine batching by default with `dispatch.routine_batch_window_secs == 5`.
2. Config loading/validation accepts overrides for the routine batch window, remains backward-compatible when omitted, and treats `0` as disabled.
3. Two or more routine Discord deliveries to the same target within the window produce one outbound send in targeted dispatcher tests.
4. Mixed routine + urgent traffic sends the urgent/failure delivery immediately while the routine delivery remains queued.
5. `github.ci-*` keeps its existing batching behavior and does not incur extra routine-batcher delay.
6. Grouped routine bursts suppress eager mentions for 2+ items, while single queued flushes keep normal mention behavior.
7. README / PR notes explain the new config field, default semantics, disable behavior, bypass rules, and mention impact.

## Implementation Steps
1. Extend `DispatchConfig` in `src/config.rs` with `routine_batch_window_secs`, add default/override/disable semantics plus validation/tests, and wire it through `src/daemon.rs` into `Dispatcher::new`.
2. In `src/dispatch.rs`, keep the existing CI path intact, then add a routine-delivery batching path that operates on resolved Discord deliveries and flushes on the existing ticker.
3. Introduce a small dispatch/router seam so grouped routine batches render bodies without mention prefixes, then apply grouped mention policy once at flush time; explicitly forbid concatenating already-mentioned rendered strings.
4. Ensure the routine batcher flushes pending deliveries during dispatcher shutdown, mirroring the existing CI flush path in `src/dispatch.rs:72-75`.
5. Add targeted dispatcher tests for grouped routine delivery, config override/disable behavior, mixed urgent bypass, CI bypass, shutdown flush, and grouped mention suppression. Update README / PR notes with exact semantics.

## Risks and Mitigations
- Risk: Delaying notifications that operators expect immediately.
  - Mitigation: Bypass batching only for explicit failure/stale/CI cases and document the v1 rule.
- Risk: Later urgent items may overtake earlier queued routine items.
  - Mitigation: Document possible ordering differences across bypassed vs queued traffic and keep the bypass list intentionally narrow.
- Risk: Mention regressions caused by concatenating already-mentioned content.
  - Mitigation: Batch before final mention prefixing and explicitly suppress mentions only for grouped routine batches.
- Risk: Config sprawl.
  - Mitigation: Add one dispatch-level field only and use `0` as the disable switch.

## Verification Steps
- Run targeted config tests for default / override / disable behavior in `src/config.rs`.
- Run targeted dispatcher tests for grouped routine delivery, mixed urgent bypass, CI bypass, shutdown flush, and mention behavior in `src/dispatch.rs`.
- Review README / PR notes to confirm exact semantics are documented.

## Available-Agent-Types Roster
- `explore` — narrow code lookups
- `planner` — plan refinement
- `architect` — design tradeoffs
- `critic` — quality gate
- `executor` — implementation
- `verifier` — completion evidence
- `test-engineer` — targeted test design
- `writer` — README / PR note wording

## Follow-up Staffing Guidance
### Ralph lane
- `executor` (high): implement `src/config.rs`, `src/daemon.rs`, `src/dispatch.rs`
- `test-engineer` (medium): tighten grouped-delivery, disable, and bypass tests
- `writer` (medium): README / PR note wording
- `verifier` (high): run targeted Rust tests and inspect outputs

### Team lane
- Lane 1: `executor` for dispatch/config code
- Lane 2: `test-engineer` for grouped-delivery and config tests
- Lane 3: `writer` or `verifier` for docs/PR notes + evidence collection

### Launch Hints
- Ralph: `$ralph Implement .omx/plans/prd-issue-122-default-batching-window.md using .omx/plans/test-spec-issue-122-default-batching-window.md`
- Team: `$team issue-122 configurable default batching window`

### Team Verification Path
1. Config parse/default/disable/override tests pass.
2. Dispatcher grouped routine delivery test passes.
3. Mixed urgent bypass and CI bypass tests pass.
4. Mention-behavior assertions pass.
5. README / PR notes describe final semantics.
