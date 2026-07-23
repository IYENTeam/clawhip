# Legacy bridge note

This directory is no longer the public integration surface.

Use the provider-native Codex or Claude hook configuration plus the generic local ingress:

```bash
op-pi native hook --provider codex --file payload.json
op-pi native hook --provider claude --file payload.json
```

Use `.op-pi/hooks/` only for additive augmentation. Routing identity now comes from git
repo/worktree discovery, not repo-local op-pi metadata files.
