---
name: dev-environment
description: Use when the user wants to set up a development environment, configure a dev shell, add project dependencies, or manage per-directory tooling.
---

# Workflow: Setting Up a Dev Environment

## Option 1: Packages only (no flake needed)

For simple projects that just need some tools on PATH:

```bash
cd ~/projects/myproject
ekapkgs env init
ekapkgs env add nodejs python3 jq
ekapkgs env allow
# Shell hook activates automatically on next cd
```

## Option 2: Flake dev shell (full Nix environment)

For projects with a `flake.nix` defining a dev shell:

```bash
cd ~/projects/myproject
ekapkgs env init
ekapkgs env flake-add .
ekapkgs env allow
# Shell hook spawns a child shell with the full dev environment
```

## Option 3: Compose multiple flakes

```bash
ekapkgs env flake-add .                          # Project's own dev shell
ekapkgs env flake-add github:user/rust-tools     # Additional tooling
ekapkgs env flake-add path:../shared-env         # Shared team env
```

Environments are merged: PATH concatenated, vars last-wins, functions unioned.

## Adding project-scoped services

```bash
ekapkgs env services add redis --template redis
ekapkgs env services add postgres --template postgres
# Start: pc -f <shadow-dir>/process-compose.yaml up
```

Services are stored in a shadow directory — nothing added to the project tree.

## Shell hook setup

Add to your shell rc (one-time):

```bash
# ~/.bashrc
eval "$(ekapkgs env hook bash)"

# ~/.zshrc
eval "$(ekapkgs env hook zsh)"

# ~/.config/fish/config.fish
ekapkgs env hook fish | source
```

The hook:
1. Detects `.ekapkgs-env.toml` when you `cd`
2. Checks trust (must `allow` first)
3. Activates: packages-only mode (PATH prepend) or dev shell (child shell spawn)
4. Deactivates when you leave the directory tree
5. Live-reloads on manifest/flake changes via fingerprint detection

## Pinning flake inputs

```bash
ekapkgs env flake-pin .                    # Pin to current revision
ekapkgs env flake-pin . --rev abc123       # Pin to specific revision
```

## Day-to-day

```bash
ekapkgs env status                # Am I in an env? What's active?
ekapkgs env list [--services]     # What's in this env?
ekapkgs env reload                # Rebuild after editing manifest/flake
ekapkgs env clean                 # Wipe cached profiles + rendered scripts
```

## Trust model

Environments require explicit trust since they execute code (Nix evaluation, env script sourcing):

```bash
ekapkgs env allow      # Trust current manifest (re-run after edits)
ekapkgs env disallow   # Revoke trust
```

Trust is a blake3 hash of the manifest contents — editing the manifest invalidates trust.
