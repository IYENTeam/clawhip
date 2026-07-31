# evidence-ledger

op_pi durable evidence ledger — AGI architecture §4.2, roadmap `M2`.

Today `POST /linear` answers `200` once an event reaches an in-memory queue, so a
crash drops in-flight events and a redelivery is processed twice. This crate makes
the acknowledgement durable: an event is committed to PostgreSQL **before** it is
acknowledged, and redeliveries collide on the `event_id` primary key so they are
absorbed instead of reprocessed.

## Status

AGI-side prototype inside the vendored `op_pi` tree. It is **not** wired into the
live intake handler yet and is **not** an owner-approved deployment. See
[`docs/roadmap.md`](../../../../docs/roadmap.md) `M2` for the gate.

## Run the tests

The integration tests need a PostgreSQL reachable via `DATABASE_URL`. They no-op
(and pass) when it is unset.

```sh
docker run -d --name agi-op-pi-pg \
  -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=op_pi_ledger \
  -p 55432:5432 postgres:16-alpine
export DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55432/op_pi_ledger
cargo test -p evidence-ledger
```

## Invariants under test

| test | invariant |
| --- | --- |
| `append_commits_then_deduplicates` | durable commit, then idempotent replay |
| `dedupe_survives_a_restart` | dedupe holds across a fresh connection pool |
| `distinct_event_ids_each_commit` | different `event_id`s each commit once |
| `empty_event_id_is_rejected` | missing idempotency key fails closed |
| `interleaved_replays_keep_exactly_one_row` | N replays leave exactly one row |
