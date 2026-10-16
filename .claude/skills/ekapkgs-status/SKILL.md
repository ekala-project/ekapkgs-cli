---
name: ekapkgs-status
description: Use when the user wants to see a unified view of all managed services, check what's running across scopes, or diagnose service state.
---

# ekapkgs Status — Unified Service View

```bash
ekapkgs status services [--json]
```

Shows all managed services across scopes in a single table:

```
SCOPE        SERVICE                  STATUS       PORT     SOURCE
home         redis                    active       6379     ekapkgs home services
env:webapp   postgres                 running      5432     ~/projects/webapp
```

- **home** scope: from `~/.config/ekapkgs/home-services.toml` + systemd status
- **env** scope: from `$EKAPKGS_ENV` + process-compose status

Also useful:
- `ekapkgs home services status` — detailed home service status
- `ekapkgs env services status` — process-compose service status
- `ekapkgs env status` — environment activation status
- `ekapkgs doctor` — system health check
