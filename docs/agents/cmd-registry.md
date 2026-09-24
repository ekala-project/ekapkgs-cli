# Command: `ekapkgs registry`

Wraps `nix registry` subcommands for managing flake registries. Registries map symbolic flake identifiers (e.g., `nixpkgs`) to full URLs (e.g., `github:NixOS/nixpkgs`). Subcommands: `list`, `add`, `remove`, `pin`, `resolve`.

- `add` and `remove` support `--registry` to operate on a specific registry file
- `pin` locks a registry entry to a specific revision
- `resolve` translates indirect flake references to direct URLs
