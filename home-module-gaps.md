# ekapkgs-cli: Home Module Gaps

Changes needed in `ekapkgs home` CLI commands to support the migration
from home-manager, assuming core-pkgs introduces a separate `home.*`
namespace and `eval-home.nix`.

---

## ~~1. Default installable path~~ (resolved)

The CLI now auto-detects the installable. It probes
`.#config.home.build.activationPackage` (standalone eval) first via
`nix eval`, then falls back to `.#config.system.build.home` (full system
eval). Users can still override with an explicit positional argument.

---

## 2. Socket-activated services (gpg-agent)

**Current behavior:**
- `ekapkgs home services add/apply` generates basic `.service` units
- `build_services_nix_expr()` builds a single nix expression that
  produces `.service` files per enabled service

**Gap:** GPG agent requires:
- Socket activation (`gpg-agent.socket`, `gpg-agent-ssh.socket`,
  `gpg-agent-extra.socket`)
- `Type=notify` with `--supervised` flag
- `gpg-agent.conf` alongside the units

The current infrastructure only generates `.service` files and only
handles `restart` semantics for plain services.

**Needed:**
- Support for `.socket` unit generation alongside `.service` units
- Alternatively, let declarative home config handle gpg-agent and
  leave `home services` for simpler daemons

**Severity:** Medium

---

## 3. `home init` scaffolding

**Current behavior:** No scaffolding command. User must manually create
a flake that produces the right attribute.

**Needed:**
- `ekapkgs home init` that generates a minimal `flake.nix` +
  `home.nix` for the current user, using `eval-home.nix` from
  core-pkgs (or ekapkgs)

**Severity:** Low — nice-to-have for onboarding

---

## ~~4. Session vars sourcing~~ (partially resolved)

The CLI now prints a hint after `home switch` if `session-vars.sh`
exists but no shell rc file (`.bashrc`, `.zshrc`, `config.fish`)
appears to source it. The hint suggests either manual sourcing or
enabling `programs.bash` (added in corepkgs PR #221).

**Remaining:** `session-vars.sh` only adds `~/.ekaos-profile/bin`
(declarative packages) to PATH. Imperative packages live in
`~/.ekapkgs-packages/bin` which is not included. Ideally the nix
module would also add `~/.ekapkgs-packages/bin` to the generated
`session-vars.sh` so both mechanisms share one PATH setup.

**Severity:** Low — imperative `packages add` already prints its own
PATH hint

---

## Summary

| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| 1 | ~~Default installable path~~ | ~~Low~~ | Resolved — auto-detect with fallback |
| 2 | Socket-activated services | Medium | GPG agent needs socket units |
| 3 | `home init` scaffolding | Low | Nice-to-have |
| 4 | ~~session-vars.sh sourcing~~ | ~~Medium~~ | CLI hint added; nix-side could also add imperative dir |
