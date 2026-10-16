---
name: ekapkgs-registry
description: Use when the user wants to manage flake registries, pin or unpin flakes, add/remove registry entries, or resolve flake references.
---

# ekapkgs Registry — Flake Registry Management

```bash
ekapkgs registry list                              # List all flake registry entries
ekapkgs registry add <from> <to> [--registry <file>]    # Add/replace a flake
ekapkgs registry remove <entry> [--registry <file>]     # Remove a flake
ekapkgs registry pin <entry> [--registry <file>]        # Pin to current version
ekapkgs registry unpin <entry> [--registry <file>]      # Unpin (fall through to system/global)
ekapkgs registry resolve <refs>...                      # Resolve flake references
```
