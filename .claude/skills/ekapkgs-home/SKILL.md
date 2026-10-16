---
name: ekapkgs-home
description: Use when the user wants to manage their home configuration, home packages, home services, or user-level dotfiles/packages/services. Covers ekapkgs home switch/build/update/rollback/generations and home packages/services subcommands.
---

# ekapkgs Home — User Configuration Management

## Configuration lifecycle

```bash
ekapkgs home switch [installable] [extra...]   # Build and activate
ekapkgs home build [installable] [extra...]    # Build without activating
ekapkgs home update [installable] [extra...]   # Update flake inputs + rebuild
ekapkgs home generations                        # List generations
ekapkgs home rollback                           # Roll back to previous generation
```

When no installable is given, tries `.#config.home.build.activationPackage` first (standalone eval), then `.#config.system.build.home` (full system eval).

## Packages

```bash
ekapkgs home packages add <packages>... [--flake <flake>]
ekapkgs home packages remove <packages>...
ekapkgs home packages list [--json] [--verbose]
ekapkgs home packages present <packages>...  # exit 0 if all present
ekapkgs home packages missing <packages>...  # exit 0 if all missing
ekapkgs home packages export [-o file]
ekapkgs home packages import <file> [--merge]
```

- Packages are stored in `~/.config/ekapkgs/home-packages.toml`
- Fast path via store path index for instant installs
- `--verbose` shows provenance metadata (when added, by whom)
- `$EKAPKGS_CALLER` env var sets the "added_by" field (default: "user")

## Services (systemd user units)

### Add/remove

```bash
ekapkgs home services add <service> [--template <name>] [--set key=value]... [--no-apply]
ekapkgs home services remove <services>... [--no-apply] [--purge]
```

Available templates: redis, postgres, mysql, mongodb, rabbitmq, nats, minio, nginx, caddy, memcached, clickhouse.

`--template` provides zero-config defaults (command, args, ports, data dir). `--set` overrides template values. `--purge` deletes `~/.local/share/<service>/` on removal.

### How services work

1. Service config is stored in `~/.config/ekapkgs/home-services.toml`
2. Services with `command` in config get direct systemd unit generation (no Nix eval needed)
3. The command binary is resolved to an absolute nix store path via `nix build`
4. A GC root symlink is created at `~/.config/ekapkgs/service-roots/<service>` to prevent garbage collection
5. The unit file is installed to `~/.config/systemd/user/<service>.service`
6. systemd manages the process

### Configure

```bash
ekapkgs home services set <service> <key> <value>
ekapkgs home services unset <service> <key>
ekapkgs home services enable <services>...
ekapkgs home services disable <services>...
```

### Apply and update

```bash
ekapkgs home services apply [--dry-run]
ekapkgs home services update [--silent] [--schema-only]
```

`update` re-resolves all service binaries to latest versions, reports changes (old -> new store path), regenerates unit files, and restarts affected services. `--silent` suppresses stdout.

### Monitor

```bash
ekapkgs home services status [service] [--json]
ekapkgs home services logs <service> [-f] [-n 100] [--since 1h]
ekapkgs home services restart <services>...
ekapkgs home services inspect <service> [--json]
```

`inspect` shows connection URL, runtime status, data directory, and full config.

### Other

```bash
ekapkgs home services list [--json] [--all]
ekapkgs home services validate [--json]
ekapkgs home services schema [service] [--json]
ekapkgs home services export [-o file]
ekapkgs home services import <file> [--merge]
ekapkgs home services linger [--enable|--disable]
```

`linger` enables loginctl linger so services persist beyond login sessions.
