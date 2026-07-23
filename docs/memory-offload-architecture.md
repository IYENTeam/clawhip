# Filesystem-Offloaded Memory Architecture

This document defines the Claw OS-style memory pattern that op-pi recommends for filesystem-backed project memory: keep `MEMORY.md` small and high-signal, and offload detailed memory into structured filesystem documents.

## Goal

Use the filesystem as the durable memory substrate while keeping the root memory surface fast to load, easy to maintain, and safe for repeated agent use.

In this pattern:

- `MEMORY.md` is the hot pointer/index layer
- `memory/` holds the detailed memory shards
- agents read the minimum set of files needed for the current task
- agents write detailed updates to leaf files, not back into a monolith
- memory refactoring/offloading is ongoing maintenance, not a one-time cleanup

This is the memory model that fits op-pi's broader direction as an OS-like runtime: small control surfaces, explicit routing, and durable state outside the hot path.

## Design principles

1. **Keep the hot layer small.** `MEMORY.md` should stay short enough to scan quickly.
2. **Shard by stable retrieval paths.** Organize memory by entity, domain, or time instead of one narrative file.
3. **Separate index from detail.** Index files answer where to read and write; leaf files hold the detail.
4. **Prefer append at the edge.** Daily logs and entity files absorb detail so the root stays curated.
5. **Refactor memory continuously.** When a section grows noisy, split it into a dedicated file and leave a pointer behind.
6. **Protect private or sensitive state.** Not every shard should be loaded in every context.

## Layer model

### Layer 1: `MEMORY.md` (hot pointer layer)

`MEMORY.md` should answer only high-value questions such as:

- what is currently true
- which files matter right now
- where a new update should be written
- what an agent must read before acting

Recommended contents:

- current beliefs / active focus
- quick file map
- scenario-based read guide
- write obligations
- recent refactors / moved files

Avoid putting long transcripts, raw logs, or exhaustive histories here.

### Layer 2: subtree indexes (routing layer)

Subtree index files live under `memory/` and narrow retrieval further.

Examples:

- `memory/README.md`
- `memory/channels/README.md`
- `memory/projects/README.md`
- `memory/agents/README.md`

Their job is to answer:

- which shard is canonical for a category
- naming conventions
- lookup rules and aliases
- which files are active vs archived

### Layer 3: leaf memory files (detail layer)

Leaf files hold the durable detail.

Common shard types:

- **daily logs** — chronological activity and handoff notes
- **channel memory** — one file per channel or conversation lane
- **project memory** — repo-specific state, plans, blockers, decisions
- **agent memory** — preferences, roles, working patterns, handoff expectations
- **topic memory** — rules, lessons, ops, people, research, decisions
- **archive files** — older daily or project material moved out of the hot set

## Recommended directory layout

A practical default layout:

```text
MEMORY.md
memory/
  README.md
  daily/
    YYYY-MM-DD.md
  channels/
    README.md
    <channel-slug>.md
  projects/
    README.md
    <project-slug>.md
  agents/
    README.md
    <agent-slug>.md
  topics/
    README.md
    rules.md
    lessons.md
    ops.md
    people.md
  decisions/
    YYYY-MM-DD-<slug>.md
  handoffs/
    YYYY-MM-DD-<slug>.md
  archive/
    YYYY-MM/
      YYYY-MM-DD.md
  registry/
    channel-registry.json
    project-registry.json
```

Notes:

- teams may use `memory/daily/YYYY-MM-DD.md` or `memory/YYYY-MM-DD.md`; standardize on one active convention
- use registries only where alias/ID lookup is genuinely useful
- avoid creating dozens of tiny folders before retrieval rules are clear

## Read path

Recommended read order:

1. open `MEMORY.md`
2. follow the scenario-based pointer to the relevant subtree index or leaf file
3. read only the files needed for the task
4. avoid bulk-loading the whole memory tree unless explicitly required

Example:

```text
Need current repo status?
-> MEMORY.md
-> memory/projects/op-pi.md
-> latest daily file if recent execution context matters
```

## Write path

Recommended write order:

1. decide the canonical target file
2. write detail into the leaf file
3. update `MEMORY.md` only if the pointer map or current beliefs changed
4. archive or split a file when it becomes noisy

Example event-driven routing:

- new execution log -> today's daily file
- channel-specific decision -> that channel file
- durable workflow rule -> `memory/topics/rules.md`
- reusable lesson -> `memory/topics/lessons.md`
- long section extracted from root memory -> dedicated shard + short pointer in `MEMORY.md`

## Offload/refactor rules

Offload content out of `MEMORY.md` when any of these are true:

- a section becomes mostly historical detail
- the content belongs to one stable entity or topic
- the content is needed only in specific workflows
- the root file is getting slow or noisy to scan
- the content is append-heavy and better suited to a log

When offloading:

1. create the destination file
2. move or summarize the detailed content there
3. replace the old root section with a one-line pointer and current takeaway
4. add or update a subtree index if the new area will grow

## op-pi-as-OS fit

op-pi already models the world as routed events, normalized contracts, and explicit sinks. The offloaded memory pattern applies the same operating idea to project state:

- `MEMORY.md` behaves like a control-plane index
- filesystem shards behave like durable state partitions
- agent workflows route reads/writes to the right partition
- archival keeps the hot surface operationally cheap

That makes memory a first-class operating pattern instead of an accidental giant note.

## Non-goals

This pattern does **not** require op-pi to become a database, vector store, or embedded note service.

It is a documentation and workflow architecture for filesystem-backed memory that agents and operators can adopt around op-pi.

## Related docs

- [Filesystem-Offloaded Memory Guide](memory-offload-guide.md)
- [Example pointer file](examples/MEMORY.example.md)
- [Example memory subtree index](examples/memory/README.example.md)
- [Memory-offload skill guide](../skills/memory-offload/SKILL.md)
- [Installable workflow skill](../skills/memory-offload/SKILL.md)
