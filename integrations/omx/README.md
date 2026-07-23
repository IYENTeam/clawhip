# Legacy bridge note

This directory is no longer the public integration surface.

Use the provider-native Codex or Claude hook configuration plus the generic local ingress:

```bash
op_pi native hook --provider codex --file payload.json
op_pi native hook --provider claude --file payload.json
```

Use `.op_pi/hooks/` only for additive augmentation. Routing identity now comes from git
repo/worktree discovery, not repo-local op_pi metadata files.
