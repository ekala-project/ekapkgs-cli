---
name: ekapkgs-cache
description: Use when the user wants to configure, manage, push to, or pull from a binary cache. Covers ekapkgs cache add/remove/list/push/pull/warm/status and auth subcommands.
---

# ekapkgs Cache Management

## Register a cache

```bash
ekapkgs cache add <name> <url>
# Example: ekapkgs cache add myteam http://cache.example.com:8080
```

## Remove a cache

```bash
ekapkgs cache remove <name>
```

## List configured caches

```bash
ekapkgs cache list
```

## Push store paths to a cache

```bash
ekapkgs cache push [--cache <name>] <paths>...
# --sources-only  transfer only sources (low-bandwidth links)
# Example: ekapkgs cache push /nix/store/abc...-hello-2.10
# Example: ekapkgs cache push --cache myteam nixpkgs#hello
```

## Pull (pre-fetch) store paths

```bash
ekapkgs cache pull [--cache <name>] <paths>...
```

## Pre-warm cache from flake.lock diff

```bash
ekapkgs cache warm <installable> --from-flake-lock-diff HEAD~1..HEAD
# Or with explicit old/new:
ekapkgs cache warm <installable> --old ./old-flake.lock --new ./flake.lock
```

## Cache server status

```bash
ekapkgs cache status [--cache <name>]
```

## Authentication

```bash
# Login with a push token
ekapkgs cache auth login <cache> --token <token>

# Logout (remove credentials)
ekapkgs cache auth logout <cache>

# Show auth status
ekapkgs cache auth status

# Mint a new push token (admin)
ekapkgs cache auth token <cache> --admin-token <token> [--name <label>]
```

## Configuration

Caches are stored in `~/.config/ekapkgs/config.toml`:

```toml
[[caches]]
name = "myteam"
url = "http://cache.example.com:8080"
priority = 10
# token = "..." (set via auth login)
```

## Environment variables

- `EKAPKGS_CACHE_URL` — fallback cache URL when no cache name is given
- `EKAPKGS_TOKEN` — fallback push token (overrides per-cache config)
