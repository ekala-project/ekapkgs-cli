# Command: `ekapkgs search`

Searches packages, configuration options, or files using ZSTD-compressed JSON indexes cached at `~/.cache/ekapkgs/indexes/`. Indexes auto-generate on first use via nix evaluation, or can be downloaded from a remote URL. File search integrates with `nix-locate` when available.

Related ekaos file: `lib/generate-options-index.nix` for option index generation.
