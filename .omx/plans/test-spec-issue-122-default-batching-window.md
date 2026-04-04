# Test Spec — issue #122 configurable default batching window

## Scope
Verify the first slice of configurable routine batching without reopening source-specific debounce systems.

## Test Matrix
### 1. Config defaults and overrides (`src/config.rs`)
- Assert `dispatch.routine_batch_window_secs` defaults to `5`.
- Assert config loading preserves backward compatibility when the field is omitted.
- Assert explicit overrides parse correctly.
- Assert `0` disables routine batching cleanly.
- Assert validation still rejects any remaining invalid values if the implementation adds bounds beyond disable semantics.

### 2. Routine dispatcher grouped delivery (`src/dispatch.rs`)
- Add a targeted webhook-backed async test that sends multiple routine Discord deliveries to the same target within the window and asserts only one outbound request is made.
- Assert grouped message content contains all constituent routine items in stable order.
- Assert the configured shorter test window flushes promptly.

### 3. Mixed urgent/failure bypass (`src/dispatch.rs`)
- Add a test showing a bypass-class delivery sends immediately even while routine batching is enabled.
- Initial bypass classes for v1:
  - event kinds ending in `.failed`
  - event kinds ending in `.blocked`
  - `tmux.stale`
  - all `github.ci-*` deliveries (they should keep existing CI batching behavior only)

### 4. Delivery-signature isolation (`src/dispatch.rs` and/or `src/router.rs`)
- Assert deliveries that differ by mention/template/dynamic-token policy do not collapse into the same routine batch.

### 5. Mention behavior (`src/dispatch.rs` and/or `src/router.rs`)
- Assert grouped routine bursts with 2+ items suppress the mention entirely.
- Assert a queued routine batch that flushes with only 1 item preserves the existing mention behavior.

## Acceptance Mapping
- AC1/AC2 map to config tests.
- AC3 maps to routine grouped-delivery dispatcher tests.
- AC4/AC5 map to urgent/failure and CI bypass tests.
- AC6 maps to delivery-signature and mention-behavior assertions.
- AC7 maps to README / PR-note review.

## Verification Commands
- `cargo test dispatch::tests::dispatcher_batches_`
- `cargo test config::tests::dispatch_`
- If test names differ, run targeted equivalents covering dispatch/config only.

## Notes
- Keep tests deterministic by using short millisecond-scale windows in dispatcher tests.
- Prefer webhook-backed integration-style tests already used in `src/dispatch.rs` over introducing new test infrastructure.
- Document the final bypass rule in code comments and PR notes so reviewers can verify the intended urgency boundary.

### 6. Shutdown flush (`src/dispatch.rs`)
- Add a test proving pending routine deliveries flush when the dispatcher channel closes.
