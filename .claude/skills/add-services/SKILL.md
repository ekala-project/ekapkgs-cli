---
name: add-services
description: Use when the user wants to add a service like Redis, Postgres, or any background process. Covers both home services (systemd, persistent) and env services (process-compose, project-scoped).
---

# Workflow: Adding and Managing Services

Two scopes for services:

## Home services (systemd, persistent, user-wide)

Best for services you always want running (Redis for all projects, personal API, etc.).

### Quick start with a template

```bash
ekapkgs home services add redis --template redis
# That's it. Redis is now running on port 6379.
```

Available templates: redis, postgres, mysql, mongodb, rabbitmq, nats, minio, nginx, caddy, memcached, clickhouse.

### What happens behind the scenes

1. Template populates command, args, ports, health_check, stop_signal
2. `valkey-server` binary is resolved via `nix build nixpkgs#valkey`
3. A GC root at `~/.config/ekapkgs/service-roots/redis` prevents garbage collection
4. A systemd unit file is generated at `~/.config/systemd/user/redis.service`
5. systemd starts the service

### Custom service (no template)

```bash
ekapkgs home services add my-api \
  --set command=my-api-server \
  --set args="--port 3000 --bind 127.0.0.1" \
  --set ports.http.port=3000 \
  --set ports.http.protocol=tcp
```

### Override template defaults

```bash
ekapkgs home services add redis --template redis --set ports.redis.port=6380
```

### Manage running services

```bash
ekapkgs home services status [service]     # Runtime status
ekapkgs home services logs redis [-f]      # View logs (journalctl)
ekapkgs home services restart redis        # Restart
ekapkgs home services inspect redis        # Full config + connection info
```

### Update to latest versions

```bash
ekapkgs home services update [--silent]
# Re-resolves binaries via nix build, reports old->new versions, restarts
```

### Remove

```bash
ekapkgs home services remove redis         # Stop + remove unit
ekapkgs home services remove redis --purge # Also delete ~/.local/share/redis/
```

## Env services (process-compose, project-scoped)

Best for services tied to a specific project (project-local Postgres, dev SMTP, etc.).

### Quick start

```bash
ekapkgs env services add postgres --template postgres
# Config stored in shadow dir, not in the project tree
# Start: pc -f <path>/process-compose.yaml up
```

### What happens

1. Template populates command, args, port, health_check, stop_signal, working_dir
2. Config stored in `~/.config/ekapkgs/shadows/<hash>/services.toml`
3. process-compose YAML generated with readiness probes and shutdown signals
4. Data directory defaults to `<git-root>/.dev/data/<service>/`
5. The git root is detected via `git rev-parse --show-toplevel`

### Manage

```bash
ekapkgs env services list [--json]
ekapkgs env services status [--json]
ekapkgs env services restart <service>
ekapkgs env services remove <service>
```

## Choosing between home and env services

| | Home services | Env services |
|---|---|---|
| **Scope** | User-wide | Per-project |
| **Runtime** | systemd | process-compose |
| **Persists** | Across reboots (with linger) | Only while pc is running |
| **Config location** | ~/.config/ekapkgs/ | Shadow dir (no project tree pollution) |
| **Best for** | Always-on services | Project-specific dev services |

## Unified view

```bash
ekapkgs status services    # All services across all scopes
```
