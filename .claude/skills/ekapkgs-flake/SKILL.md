---
name: ekapkgs-flake
description: Use when the user wants to inspect flake outputs, view flake metadata, or diff flake input updates.
---

# ekapkgs Flake Introspection

```bash
ekapkgs flake show [flake_ref]       # Pretty-print flake outputs tree (default: .)
ekapkgs flake metadata [flake_ref]   # Show flake input dependency tree (default: .)
ekapkgs flake update-diff <input>    # Update a flake input and show closure size diff
                                     # --installable <ref>  what to evaluate (default: .)
```
