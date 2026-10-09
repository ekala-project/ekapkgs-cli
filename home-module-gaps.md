# ekapkgs-cli Home Gaps

Gaps in the `ekapkgs home` CLI commands that affect the migration experience.

Last reviewed: 2026-10-09

---

## 1. Default installable assumes full ekaos system evaluation — RESOLVED

**Status:** Resolved

`resolve_home_installable()` in `commands/home.rs` now implements a
two-step probe:
1. First tries `.#config.home.build.activationPackage` (standalone eval)
2. Falls back to `.#config.system.build.home` (full system eval)
3. Users can override with an explicit positional argument

The standalone `eval-home.nix` path is supported and preferred.

---

## 2. Socket-activated services — RESOLVED

**Status:** Resolved

The service infrastructure now handles `.service`, `.socket`, and `.timer`
unit files as a group. Changes made in `commands/home.rs`:

- `installed_managed_units()` scans for all three unit suffixes and maps
  unit files back to logical service names via a `service=<name>` tag in
  the managed marker comment.
- `installed_managed_unit_files()` (new) returns all managed unit file
  names for removal/cleanup operations.
- `build_services_nix_expr()` copies all `.service`, `.socket`, and
  `.timer` files from the build output instead of hardcoding a single
  `{name}.service` file.
- `cmd_services_apply()` installs/removes all unit types, tags each with
  its owning logical service, cleans up stale units no longer produced by
  the build, and uses the correct start strategy (socket-activated services
  start sockets first; timer-triggered services start timers).
- `cmd_services_restart()` discovers and restarts all managed unit files
  belonging to the service, not just the `.service` unit.

Helper functions `is_managed_unit_file()`, `strip_unit_suffix()`, and the
`MANAGED_UNIT_SUFFIXES` constant centralise the set of supported unit types.

---

## 3. No `home init` or scaffolding command — OPEN

**Current behavior:**
- User must know the expected flake structure and manually create a flake
  that produces `config.home.build.activationPackage`

**Gap:** No `ekapkgs home init` command that scaffolds a basic home
configuration flake (analogous to `home-manager init`).

**Severity:** Low — nice-to-have for onboarding

---

## 4. Session vars sourcing — PARTIALLY RESOLVED

**Status:** Partially resolved

**What is resolved:** `hint_session_vars()` is called after every
successful `home switch`. It checks if `.bashrc`, `.zshrc`, or
`.config/fish/config.fish` source `session-vars.sh`, and prints a hint
if not.

**Remaining gap:** `session-vars.sh` only adds `~/.ekaos-profile/bin`
(declarative packages) to PATH. Imperatively installed packages via
`ekapkgs home packages add` live in `~/.ekapkgs-packages/bin` and are
not included. This is a nix-module-side issue, not a CLI issue.

**Severity:** Low — the `packages add` command has its own separate PATH
hint

---

## Summary

| # | Gap | Severity | Status |
|---|-----|----------|--------|
| 1 | Default installable path | — | Resolved |
| 2 | Socket-activated services | — | Resolved |
| 3 | No `home init` scaffolding | Low | Open |
| 4 | session-vars.sh sourcing | Low | Partially resolved (hint works) |

**No blockers.** All remaining gaps are low severity with functional
workarounds.
