---
name: ekapkgs-search
description: Use when the user wants to search for nix packages, configuration options, or files. Covers ekapkgs search packages/options/files/update.
---

# ekapkgs Search

## Search packages

```bash
ekapkgs search packages [query] [--flake nixpkgs] [--json] [--names-only] [--limit 20]
# Empty query returns ALL packages (useful for piping to fzf)
# Scoring: exact > prefix > contains > description > fuzzy
# Example: ekapkgs search packages python
# Example: ekapkgs search packages | fzf
# Example: ekapkgs search packages --names-only "http" | head
```

## Search options

```bash
ekapkgs search options <query> [--flake .] [--json] [--limit 20]
# Searches NixOS/ekaos configuration options by name or description
```

## Search files

```bash
ekapkgs search files <query> [--json] [--names-only] [--limit 20]
# Find which package provides a file
# --names-only output is valid input for `home packages remove`
# Falls back to nix-locate if no cached index
```

## Update indexes

```bash
ekapkgs search update [--flake nixpkgs] [--remote <url>]
# --remote downloads pre-built indexes from CI instead of generating locally
# Indexes are stored at ~/.cache/ekapkgs/indexes/ as ZSTD-compressed JSON
```

## Index infrastructure

- Package index: `nix search <flake> --json ^` -> ZSTD compressed
- Option index: nix eval with `lib.optionAttrSetToDocList`
- File index: cached or downloaded, falls back to `nix-locate`
- Remote index URL configurable via `index_url` in `~/.config/ekapkgs/config.toml`
