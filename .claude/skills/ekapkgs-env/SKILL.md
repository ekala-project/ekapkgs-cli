---
name: ekapkgs-env
description: Use when the user wants to manage directory-scoped package environments, dev shells, per-project packages, or ekapkgs env commands. Covers init, add, remove, flake management, services, status, reload, clean, allow/disallow, and shell hooks.
---

# ekapkgs Env — Directory-Scoped Environments

## Initialize

```bash
ekapkgs env init [--flake nixpkgs]
# Creates .ekapkgs-env.toml in the current directory
```

## Packages

```bash
ekapkgs env add <packages>... [--flake <flake>]
ekapkgs env remove <packages>...
ekapkgs env list [--json] [--services]
```

## Flake dev shells

```bash
ekapkgs env flake-add <ref> [--devshell default] [--rev <rev>] [--override-input name=ref]
ekapkgs env flake-remove <ref>
ekapkgs env flake-pin <ref> [--rev <rev>]
```

## Services (process-compose)

Services are stored in a shadow directory (`~/.config/ekapkgs/shadows/<hash>/`), not in the project tree. The env root defaults to the git root (`git rev-parse --show-toplevel`).

```bash
ekapkgs env services add <service> [--template redis|postgres|...] [--set key=value]
ekapkgs env services remove <services>...
ekapkgs env services list [--json]
ekapkgs env services status [--json]
ekapkgs env services restart <services>...
```

Available templates: redis, postgres, mysql, mongodb, rabbitmq, nats, minio, nginx, caddy, memcached, clickhouse.

Templates populate command, args, port, health_check, stop_signal, and working_dir. The generated process-compose YAML includes readiness probes and shutdown signals.

Start services with: `pc -f <shadow-dir>/process-compose.yaml up`

## Environment status

```bash
ekapkgs env status [--json]
# Shows: active (from $EKAPKGS_ENV), manifest path, trust status, counts
```

## Lifecycle

```bash
ekapkgs env reload     # Rebuild profile from manifest + flake state
ekapkgs env clean      # Remove cached profiles and rendered env scripts
ekapkgs env allow      # Trust the current directory's manifest
ekapkgs env disallow   # Remove trust
```

## Shell hook

```bash
# Add to shell rc:
eval "$(ekapkgs env hook bash)"   # bash
eval "$(ekapkgs env hook zsh)"    # zsh
ekapkgs env hook fish | source    # fish
```

The hook automatically activates/deactivates environments when you `cd` into or out of directories with `.ekapkgs-env.toml`. Flake-containing manifests spawn a child shell with the full dev environment.

## How it works

- **Packages-only** (no flakes): prepends profile bin to PATH
- **Dev shell** (has flakes): spawns child shell sourcing rendered env scripts from `nix print-dev-env`
- **Trust**: manifest must be trusted via `allow` before the hook activates it
- **Fingerprinting**: changes to manifest/flake.nix/flake.lock trigger automatic re-render
- **Shadow dirs**: service configs live in `~/.config/ekapkgs/shadows/` keyed by git root path hash
