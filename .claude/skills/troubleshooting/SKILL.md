---
name: troubleshooting
description: Use when the user is having problems with ekapkgs, a service won't start, a package is missing, an environment won't activate, or something is broken.
---

# Troubleshooting ekapkgs

## System health check

```bash
ekapkgs doctor
```

## Service won't start

```bash
# Check status
ekapkgs home services status <service>

# Check logs
ekapkgs home services logs <service> -n 50

# Inspect full config
ekapkgs home services inspect <service>

# Check the unit file directly
cat ~/.config/systemd/user/<service>.service

# Check systemd journal
journalctl --user -u <service>.service -e

# Common causes:
# - Binary not found: the store path was garbage collected
#   Fix: ekapkgs home services update (re-resolves + creates new GC root)
# - Port in use: another process is on the same port
#   Fix: ekapkgs home services set <service> ports.<label>.port <new-port>
#        ekapkgs home services apply
# - Permission denied: check file/directory permissions
# - Missing data dir: create it manually or re-add with --template
```

## Package not found after install

```bash
# Check if it's in the manifest
ekapkgs home packages present <package>

# Check PATH
echo $PATH | tr ':' '\n' | grep ekapkgs

# For home packages, ensure PATH includes:
#   $HOME/.ekapkgs-packages/bin

# For env packages, ensure the shell hook is active:
echo $EKAPKGS_ENV  # should show the directory path

# Rebuild the store path index
ekapkgs home switch  # or system switch
```

## Environment won't activate

```bash
# Check status
ekapkgs env status

# Is the manifest trusted?
ekapkgs env allow

# Is the shell hook installed?
# Check your shell rc for: eval "$(ekapkgs env hook <shell>)"

# Force rebuild
ekapkgs env reload

# Nuclear option: clean and rebuild
ekapkgs env clean
ekapkgs env reload
ekapkgs env allow
```

## Store path was garbage collected

```bash
# For services: re-resolve binaries (creates new GC roots)
ekapkgs home services update

# For packages: re-apply
ekapkgs home switch  # or system switch

# Check GC roots
ls -la ~/.config/ekapkgs/service-roots/
```

## Nix build failures

```bash
# See what nix would do
ekapkgs dryrun <installable>

# Check build logs
ekapkgs log <installable>

# Trace dependency issues
ekapkgs closure why-depends <package> <dependency>

# Diff what changed
ekapkgs closure diff <old> <new>
```

## Cache issues

```bash
# Check connectivity
ekapkgs cache status

# Check auth
ekapkgs cache auth status

# Try without cache (remove from config temporarily)
# Or use the local substituter for debugging:
ekapkgs substituter --port 7422
```

## Shadow directory issues (env services)

```bash
# Find the shadow dir for the current project
ls ~/.config/ekapkgs/shadows/

# Check the meta.toml for the recorded git root
cat ~/.config/ekapkgs/shadows/<hash>/meta.toml

# Clean up stale shadow dirs
rm -rf ~/.config/ekapkgs/shadows/<hash>/
```

## Reset everything

```bash
# Remove all home services
ekapkgs home services list --json | jq -r '.[].name' | xargs ekapkgs home services remove

# Remove all home packages
ekapkgs home packages list --json | jq -r '.[].name' | xargs ekapkgs home packages remove

# Clean env caches
ekapkgs env clean

# Garbage collect
ekapkgs store gc
```
