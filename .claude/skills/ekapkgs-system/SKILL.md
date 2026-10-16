---
name: ekapkgs-system
description: Use when the user wants to manage the NixOS system configuration, system rebuild, system packages, generations, rollback, boot entries, or system updates. Replaces nixos-rebuild.
---

# ekapkgs System — NixOS Configuration Management

## Configuration lifecycle

```bash
ekapkgs system switch [installable] [--channel <ch>] [--dry-run] [extra...]
ekapkgs system boot [installable] [--channel <ch>] [extra...]
ekapkgs system test [installable] [extra...]
ekapkgs system build [installable] [-p <profile>] [extra...]
ekapkgs system diff [installable] [extra...]
ekapkgs system update-check [installable] [--json] [extra...]
ekapkgs system update [installable] [extra...]
```

- **switch**: build, set profile, activate, update boot entry
- **boot**: build + boot entry only, activate on next reboot
- **test**: activate now without updating boot entry
- **build**: build only, optionally record in a named profile
- **diff**: compare new build against /run/current-system
- **update-check**: check if updates available (exit 0 = yes, 1 = up to date)
- **update**: `nix flake update` then switch

When no installable is given, tries `.#ekaosConfigurations.<hostname>` first, then `.#config.system.build.toplevel`.

`--channel` switches the flake input to a specific branch before building (requires `channel_input` and `channel_url` in config).

## Generations

```bash
ekapkgs system list-generations [--json] [-p <profile>]
ekapkgs system rollback [generation] [--dry-run] [-p <profile>]
ekapkgs system prune-boot-entries [--boot-mount /boot] [--gc] [--dry-run]
```

`prune-boot-entries` removes orphaned BLS entries, kernel/initrd, and UKI files from the ESP after garbage collection.

## System packages

```bash
ekapkgs system packages add <packages>... [--flake <flake>]
ekapkgs system packages remove <packages>...
ekapkgs system packages list [--json] [--verbose]
ekapkgs system packages present <packages>...
ekapkgs system packages missing <packages>...
ekapkgs system packages export [-o file]
ekapkgs system packages import <file> [--merge]
```

System packages use `sudo nix profile install --profile /nix/var/nix/profiles/ekapkgs-system-packages`. Provenance metadata (added_at, added_by) is recorded with `--verbose` display.

## Auto-rollback

Both `system switch` and `home switch` perform auto-rollback on activation failure: the previous profile is restored and re-activated.
