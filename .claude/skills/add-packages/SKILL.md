---
name: add-packages
description: Use when the user wants to install packages, add software, or manage installed packages across home, system, or dev environments.
---

# Workflow: Adding Packages

There are three scopes for package installation:

## Home packages (user profile, persists across sessions)

```bash
# Add
ekapkgs home packages add <packages>... [--flake <flake>]
# Example: ekapkgs home packages add ripgrep fd jq

# Remove
ekapkgs home packages remove <packages>...

# List
ekapkgs home packages list [--verbose]  # --verbose shows when/by whom
```

Stored in `~/.config/ekapkgs/home-packages.toml`. Uses fast path via store path index when available.

## System packages (root-level, persists across reboots)

```bash
ekapkgs system packages add <packages>... [--flake <flake>]
ekapkgs system packages remove <packages>...
ekapkgs system packages list [--verbose]
```

Requires sudo. Stored in `~/.config/ekapkgs/system-packages.toml`, uses profile at `/nix/var/nix/profiles/ekapkgs-system-packages`.

## Environment packages (project-scoped, activates on cd)

```bash
ekapkgs env init                         # Create .ekapkgs-env.toml
ekapkgs env add <packages>...            # Add to current directory env
ekapkgs env remove <packages>...
ekapkgs env allow                        # Trust the manifest
```

Activates automatically via shell hook when you `cd` into the directory.

## Ephemeral use (nothing persisted)

```bash
ekapkgs try <packages>... [-- command args...]
# Example: ekapkgs try imagemagick -- convert in.png out.jpg
```

## Checking package state

```bash
ekapkgs home packages present jq ripgrep   # exit 0 if all present
ekapkgs home packages missing jq ripgrep   # exit 0 if all missing
ekapkgs search packages <query>             # find packages
```

## Syncing between machines

```bash
# Export from source machine:
ekapkgs home packages export -o packages.toml

# Import on target machine:
ekapkgs home packages import packages.toml [--merge]
```

## Provenance tracking

Packages record `added_at` (timestamp) and `added_by` (from `$EKAPKGS_CALLER` or "user"). View with `--verbose`:

```
NAME                     ADDED            BY                   FLAKE
ripgrep                  2026-10-15       user                 nixpkgs
valkey                   2026-10-15       claude-code          nixpkgs
```
