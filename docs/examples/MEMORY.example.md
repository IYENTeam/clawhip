# MEMORY.md — pointer/index layer

## Current beliefs

- Current priority: stabilize project work, keep memory retrieval cheap, and offload detail into filesystem shards.
- Root memory is for summaries, pointers, and write obligations only.
- Detailed logs belong in `memory/`.

## Quick file map

- Project status: `memory/projects/op_pi.md`
- Today's execution log: `memory/daily/2026-03-10.md`
- Channel-specific state: `memory/channels/example-channel.md`
- Durable rules and lessons: `memory/topics/rules.md`, `memory/topics/lessons.md`
- Full subtree guide: `memory/README.md`

## Read this when...

- You need current repo status -> read `memory/projects/op_pi.md`
- You need latest execution context -> read today's file in `memory/daily/`
- You are acting in one channel/lane -> read that file in `memory/channels/`
- You are changing workflow policy -> read `memory/topics/rules.md`

## Write obligations

- Daily progress goes to today's daily file.
- Channel-specific detail goes to that channel file.
- Durable lessons get promoted into `memory/topics/lessons.md`.
- `MEMORY.md` only changes when the pointer map or current beliefs change.
