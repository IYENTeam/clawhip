# Legacy bridge note

This directory is no longer the public integration surface.

Use the provider-native Codex or Claude hook configuration plus the generic local ingress:

```bash
clawhip native hook --provider codex --file payload.json
clawhip native hook --provider claude --file payload.json
```

Keep stable project metadata in `.clawhip/project.json` and use `.clawhip/hooks/` only for
additive augmentation.
