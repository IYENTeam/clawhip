# evidence-ledger

op_pi durable evidence ledger — AGI architecture §4.2, roadmap `M2`.

Without `OP_PI_DATABASE_URL`, `POST /linear` keeps the legacy volatile path and
answers `200` after in-memory queue admission. With the setting present, this crate
commits the signed-body dedupe record to PostgreSQL **before** `200`; redeliveries
collide on the durable key and ledger or queue failure returns `503`.

## Status

AGI-side prototype inside the vendored `op_pi` tree. The daemon wires it into
`POST /linear` only when `OP_PI_DATABASE_URL` is set; the unset mode remains
volatile. This conditional wiring is **not** an owner-approved deployment. See
[`docs/roadmap.md`](../../../../docs/roadmap.md) `M2` for the gate.

## Run the tests

The 28 integration tests require PostgreSQL via `DATABASE_URL`. They fail closed
when the variable is missing, the backend is unreachable, migrations fail, or a
readiness round trip cannot complete. A green result therefore means all 28 tests
executed against a live backend; it is not a skip-compatible gate.

```sh
docker run -d --name agi-op-pi-pg \
  -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=op_pi_ledger \
  -p 55432:5432 postgres:16-alpine
export DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55432/op_pi_ledger
cargo test -p evidence-ledger
```

Inbox and accepted-receipt identities are bound to domain-separated payload
hashes. Transactional accept also binds the ordered outbox intents; legacy rows
without these hashes fail closed on replay. Relay workers claim disjoint rows
with expiring PostgreSQL leases, record every failed attempt, and quarantine a
poison row after three attempts without blocking later rows.

## Invariants under test

| test | invariant |
| --- | --- |
| `append_commits_then_deduplicates` | durable commit, then idempotent replay |
| `dedupe_survives_a_restart` | dedupe holds across a fresh connection pool |
| `distinct_event_ids_each_commit` | different `event_id`s each commit once |
| `empty_event_id_is_rejected` | missing idempotency key fails closed |
| `interleaved_replays_keep_exactly_one_row` | N replays leave exactly one row |
| `duplicate_accept_with_conflicting_payload_fails_closed` | inbox and ordered outbox content are immutable under one event id |
| `concurrent_drainers_claim_disjoint_entries` | concurrent relay workers never claim the same live lease |
| `poison_entry_is_quarantined_without_blocking_later_rows` | retries are auditable and poison rows cannot stall the backlog |
