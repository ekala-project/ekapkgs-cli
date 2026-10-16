---
name: system-update
description: Use when the user wants to update their system, check for updates, update packages, update services, or perform a full system upgrade.
---

# Workflow: Updating the System

## Check if updates are available

```bash
ekapkgs system update-check [--json]
# Exit 0 = updates available, exit 1 = up to date
```

## Full system update

```bash
ekapkgs system update
# Runs: nix flake update -> invalidates store path index -> system switch
```

## Home configuration update

```bash
ekapkgs home update
# Runs: nix flake update -> home switch
```

## Update services to latest versions

```bash
ekapkgs home services update [--silent]
# Re-resolves all service binaries via nix build
# Reports: redis — valkey-9.1.2 -> valkey-9.2.0 (or "unchanged")
# Regenerates unit files and restarts affected services
```

## See what would change before updating

```bash
ekapkgs system diff
# Builds new config, diffs against /run/current-system
# Shows added/removed/changed packages

ekapkgs closure sbom-diff /run/current-system .#config.system.build.toplevel
# Detailed package-level diff
```

## Channel switching

```bash
ekapkgs system switch --channel stable
ekapkgs system switch --channel unstable
# Requires channel_input and channel_url in config
```

## Update search indexes

```bash
ekapkgs search update [--remote <url>]
```

## After updating: clean up

```bash
ekapkgs store gc [--older-than 30d]
ekapkgs system prune-boot-entries [--gc]  # Clean orphaned ESP entries
ekapkgs store optimize                     # Deduplicate store
```

## Rollback if something goes wrong

```bash
ekapkgs system rollback [generation]  # Roll back system
ekapkgs home rollback                  # Roll back home config
```

Both `system switch` and `home switch` auto-rollback on activation failure.
