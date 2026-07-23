# Filesystem-Offloaded Memory Guide

This guide shows how agents and operators should use the offloaded memory pattern in practice.

For the architecture/spec, see [Filesystem-Offloaded Memory Architecture](memory-offload-architecture.md).

## Operating rule

Treat `MEMORY.md` as the fast pointer layer, not the place where all detail accumulates.

A good default policy is:

- read `MEMORY.md` first
- jump to the smallest relevant shard
- write detail into leaf files
- update root pointers only when the map or current beliefs change

## Bootstrap with op_pi

op_pi now ships a first runtime vertical slice for this pattern:

```bash
# initialize a scaffold in the current repo
op_pi memory init --project op_pi --channel discord-alerts --agent codex

# pin a specific daily shard name when backfilling or scripting
op_pi memory init --project op_pi --date 2026-03-10

# inspect the scaffold and list missing recommended paths
op_pi memory status --project op_pi --channel discord-alerts --agent codex
```

What `op_pi memory init` bootstraps:

- `MEMORY.md`
- `memory/README.md`
- `memory/daily/YYYY-MM-DD.md`
- `memory/projects/<project>.md`
- `memory/topics/rules.md`
- `memory/topics/lessons.md`
- optional `memory/channels/<channel>.md`
- optional `memory/agents/<agent>.md`

The command leaves existing files untouched by default and only overwrites scaffold files when you pass `--force`.

## What goes where

### Put in `MEMORY.md`

- active focus
- short current-state summary
- mandatory read paths for common situations
- write obligations
- links/pointers to canonical files
- recently moved or split sections

### Put in `memory/` leaf files

- detailed notes
- chronological logs
- channel-specific context
- project-specific state
- lessons, decisions, and operating rules
- handoff detail
- raw or semi-raw material that is too large for the hot layer

## Practical agent workflow

### Before acting

1. Read `MEMORY.md`.
2. Follow the scenario pointer for the current task.
3. Load only the relevant project/channel/topic/daily shard.
4. If no canonical target exists, create one in the correct subtree.

### While working

- append execution detail to the leaf shard that owns it
- keep root updates short and intentional
- if you discover a repeated retrieval path, add it to an index
- if a shard starts mixing unrelated topics, split it

### After working

- write detailed outcome to the canonical shard
- update `MEMORY.md` with only the new current belief or pointer change
- move stale time-based material to `archive/` when needed

## Recommended write-routing rules

Use rules like these:

| If the update is about... | Write to... |
|---|---|
| what happened today | `memory/daily/YYYY-MM-DD.md` |
| one Discord/Slack/channel lane | `memory/channels/<channel>.md` |
| one project/repo | `memory/projects/<project>.md` |
| one agent/operator profile | `memory/agents/<agent>.md` |
| reusable lessons | `memory/topics/lessons.md` |
| durable policies/rules | `memory/topics/rules.md` |
| one handoff | `memory/handoffs/YYYY-MM-DD-<slug>.md` |
| older inactive history | `memory/archive/...` |

## Migration: monolithic `MEMORY.md` -> offloaded memory

A safe migration path:

### 1. Freeze the role of `MEMORY.md`

Rewrite the file so it becomes:

- current beliefs
- file map
- scenario-based read guide
- write obligations

Do **not** keep adding detailed narrative after this step.

### 2. Identify high-growth sections

Typical sections to extract first:

- daily logs
- per-project sections
- per-channel sections
- long decision histories
- raw handoff dumps
- reusable rules/lessons hidden inside narrative blocks

### 3. Create the first shards

Start with the highest-leverage set:

```text
memory/README.md
memory/daily/
memory/projects/
memory/channels/
memory/topics/
memory/archive/
```

You do not need every subtree on day one.

### 4. Move detail, leave pointers

For each extracted section:

- move the detailed content into the new shard
- replace it in `MEMORY.md` with:
  - a short summary
  - the canonical file path
  - when to read it

### 5. Add write obligations

Make the system self-maintaining by stating rules such as:

- daily activity must go to today's daily file
- channel-specific context must go to the channel file
- durable lessons must be lifted into `topics/lessons.md`
- root memory must only hold summaries and pointers

### 6. Archive aggressively

Once a daily or project shard is no longer hot:

- compress it into a monthly archive bucket, or
- leave a short status summary and move the history out

## Refactor triggers

Refactor memory when:

- `MEMORY.md` stops being skimmable
- the same topic keeps expanding in the root file
- agents repeatedly read too much irrelevant context
- a file serves more than one clear owner
- retrieval depends on remembering ad hoc prose instead of stable paths

## Example starter set

Concrete example files in this repo:

- [docs/examples/MEMORY.example.md](examples/MEMORY.example.md)
- [docs/examples/memory/README.example.md](examples/memory/README.example.md)
- [docs/examples/memory/channels/example-channel.md](examples/memory/channels/example-channel.md)
- [docs/examples/memory/daily/2026-03-10.md](examples/memory/daily/2026-03-10.md)
- [skills/memory-offload/SKILL.md](../skills/memory-offload/SKILL.md)
- [docs/examples/memory/projects/op_pi.md](examples/memory/projects/op_pi.md)
- [docs/examples/memory/topics/rules.md](examples/memory/topics/rules.md)
- [docs/examples/memory/topics/lessons.md](examples/memory/topics/lessons.md)

## Cautions

- Do not turn the new tree into a second monolith.
- Do not create shards without clear read/write ownership.
- Do not expose sensitive memory in shared or automatically loaded files.
- Do not keep parallel daily-file conventions forever; pick one and normalize.
- Do not copy private production memory into public examples; abstract the pattern.

## Quick checklist

- Is `MEMORY.md` short and high-signal?
- Does every common workflow have a canonical file?
- Are daily logs separated from durable rules/lessons?
- Are archive rules clear?
- Can an agent tell where to write without guessing?
