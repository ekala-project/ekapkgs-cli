# Command: `ekapkgs system`

Replaces `nixos-rebuild` for local system configuration. Builds `system.build.toplevel` from the flake, manages `/nix/var/nix/profiles/system`, and activates via `switch-to-configuration`. Subcommands: `switch`, `boot`, `test`, `build`, `list-generations`, `rollback`, `prune-boot-entries`.

- `switch` auto-rolls back to the previous profile if activation fails
- `prune-boot-entries` removes orphaned BLS entries, kernel/initrd files, and UKI files from the ESP after generations are garbage collected
- `--gc` flag on `prune-boot-entries` runs `nix-collect-garbage -d` first
