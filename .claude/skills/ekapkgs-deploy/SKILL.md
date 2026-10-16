---
name: ekapkgs-deploy
description: Use when the user wants to deploy a NixOS configuration to a remote host.
---

# ekapkgs Deploy

Deploy a NixOS configuration to a remote host via SSH.

```bash
ekapkgs deploy <installable> --target-host <ssh-dest>
# --build-host <host>  build on a different machine (default: local)
# --mode switch|boot|test  activation mode (default: switch)
# --dry-run  only show what would be done

# Examples:
ekapkgs deploy .#nixosConfigurations.prod --target-host root@prod.example.com
ekapkgs deploy .#nixosConfigurations.staging --target-host staging --build-host builder --mode boot
```

## Modes

- **switch** — activate immediately and update boot entry (default)
- **boot** — update boot entry only, activate on next reboot
- **test** — activate now but don't update boot entry
