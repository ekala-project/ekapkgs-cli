---
name: setup-cache
description: Use when the user wants to set up a binary cache, configure cache authentication, connect to a team cache server, or optimize nix build times with caching.
---

# Workflow: Setting Up a Binary Cache

## 1. Register the cache

```bash
ekapkgs cache add <name> <url>
# Example: ekapkgs cache add team http://cache.internal:8080
```

## 2. Authenticate (if the cache requires push access)

```bash
# If you have a token:
ekapkgs cache auth login <name> --token <token>

# If you need to mint one (requires admin access):
ekapkgs cache auth token <name> --admin-token <admin-token> --name "my-workstation"
```

## 3. Verify connectivity

```bash
ekapkgs cache status --cache <name>
ekapkgs cache auth status
```

## 4. Configure as default

Edit `~/.config/ekapkgs/config.toml`:

```toml
[defaults]
max_parallel_downloads = 8
# index_url = "http://cache.internal:8080/indexes"

[[caches]]
name = "team"
url = "http://cache.internal:8080"
priority = 10
```

Lower priority numbers are checked first. The primary cache is used for pre-fetching during `home switch`, `system switch`, and `develop`.

## 5. Push to the cache

```bash
# After building:
ekapkgs cache push nixpkgs#hello
ekapkgs cache push /nix/store/abc...-mypackage

# Push sources only (for low-bandwidth links):
ekapkgs cache push --sources-only .#config.system.build.toplevel
```

## 6. Pre-warm after flake updates

```bash
ekapkgs cache warm .#config.system.build.toplevel --from-flake-lock-diff HEAD~1..HEAD
```

## Local substituter

For transparent nix integration without modifying nix.conf:

```bash
ekapkgs substituter --port 7422 [--upstream <url>]
# Then: nix build --substituters http://localhost:7422 ...
```
