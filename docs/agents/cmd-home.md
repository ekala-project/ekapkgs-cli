# Command: `ekapkgs home`

Replaces `home-manager`. Per-user dotfiles, packages, environment variables, shell aliases, and activation scripts are defined in the ekaos module system under `users.users.<name>` and built as `system.build.home`. The activation script runs as the user (no root) and manages symlinks into `$HOME` with a JSON manifest for cleanup. State stored at `~/.config/ekaos/`. Subcommands: `switch`, `build`, `generations`, `rollback`, `packages`, `services`.

- `switch` auto-rolls back to the previous generation if activation fails
- `rollback` activates the previous home generation

Related ekaos module: `modules/config/home.nix` in the `core-pkgs` repo.
