---
name: ekapkgs-store
description: Use when the user wants to manage the nix store, garbage collect, optimize, or verify store integrity.
---

# ekapkgs Store Management

```bash
ekapkgs store gc [--older-than 30d] [--dry-run]  # Garbage collect
ekapkgs store optimize                            # Deduplicate via hardlinks
ekapkgs store verify [--all] [--repair] [--sigs-needed <n>]  # Verify integrity
```

Note: after `store gc`, run `ekapkgs system prune-boot-entries` to clean up orphaned boot entries from the ESP.
