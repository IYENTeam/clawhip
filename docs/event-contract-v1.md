# Event Contract v1

This document freezes clawhip's v1 shared provider-native hook contract for Codex and Claude.

## Status

- **Schema version:** `1`
- **Stability:** stable
- **Compatibility policy:** v1 allows additive metadata only. The shared event set and base-field
  meanings are frozen.

## Shared event family

clawhip v1 supports exactly these five provider-native events:

- `SessionStart`
- `PreToolUse`
- `PostToolUse`
- `UserPromptSubmit`
- `Stop`

Provider-specific extras are out of scope for v1.

## Supported ingress

The public local ingress for shared provider-native payloads is:

```bash
clawhip native hook --provider codex --file payload.json
clawhip native hook --provider claude --file payload.json
```

## Frozen base fields

After clawhip normalizes a provider payload, these base routing fields are the v1 contract:

| Field | Required | Notes |
| --- | --- | --- |
| `provider` | yes | `codex` or `claude` for the shared v1 surface. |
| `event` | yes | One of the 5 shared event names above. |
| `directory` | when known | Provider working directory at hook time. |
| `worktree_path` | when known | Worktree/repo path used for routing and context. |
| `repo_name` | when known | Repository identity for stable routing. |
| `project` | when known | Project identity, preferably sourced from `.clawhip/project.json`. |
| `session_id` | no | Provider/session correlation identifier. |
| `branch` | when known | Git branch when available. |
| `tool_name` | tool events | Tool identifier for pre/post tool hooks. |
| `command` | tool events | Command context when a provider supplies it. |
| `summary` | no | Short human-readable context. |
| `event_timestamp` | no | Timestamp preserved from provider input when available. |

## Augmentation rules

`.clawhip/hooks/` may only add data to the normalized base contract.

Allowed:

- extra message/context fields
- frontmatter enrichment
- recent-context attachment

Disallowed:

- removing or replacing `provider`, `event`, `directory`, `worktree_path`, `repo_name`, or `project`
- replacing the entire payload with a custom schema
- promoting provider-specific extra events into the shared route surface

## Verification expectations

v1 verification must prove:

1. Codex and Claude both normalize all 5 shared events successfully.
2. Project metadata survives normalization.
3. Augmentation is additive and preserves the base contract.
4. Public documentation only references the provider-native surface.

## Relationship to the higher-level contract note

`docs/native-event-contract.md` remains the routing/augmentation guide.
This document is the frozen v1 source of truth for shared events, base fields, and compatibility
policy.
