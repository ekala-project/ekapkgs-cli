# Command: `ekapkgs system`

Replaces `nixos-rebuild` for local system configuration. Builds `system.build.toplevel` from the flake, manages `/nix/var/nix/profiles/system`, and activates via `switch-to-configuration`. Subcommands: `switch`, `boot`, `test`, `build`, `diff`, `list-generations`, `rollback`, `prune-boot-entries`, `update`, `packages`.

- `switch` auto-rolls back to the previous profile if activation fails
- `--install-bootloader` (on `switch`, `boot`) forces bootloader reinstallation by setting `NIXOS_INSTALL_BOOTLOADER=1` via `sudo env`
- `--specialisation` / `-c` (on `switch`, `boot`, `test`) activates a specific specialisation by setting `NIXOS_SPECIALISATION` via `sudo env`; validates the specialisation path exists before activation
- `--profile-name` / `-p` (on `switch`, `boot`, `test`, `build`, `list-generations`, `rollback`) uses an alternate profile under `/nix/var/nix/profiles/system-profiles/<name>`; creates the parent directory if needed
- `rollback` accepts an optional generation number to roll back to a specific generation (without it, rolls back one step)
- `prune-boot-entries` removes orphaned BLS entries, kernel/initrd files, and UKI files from the ESP after generations are garbage collected
- `--gc` flag on `prune-boot-entries` runs `nix-collect-garbage -d` first
