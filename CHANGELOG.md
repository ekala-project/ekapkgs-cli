# Changelog

## Unreleased

### Client

- `home rollback` command to revert to the previous home configuration generation
- `home generations` output now marks the current generation with `(current)`
- Auto-rollback on activation failure: `system switch` restores the previous profile and re-activates it; `home switch` re-activates the previous generation
- `deploy` prints a recovery command when remote activation fails
- `home services apply` reports which services failed to start instead of silently continuing
- Manifest writes are now atomic (write-then-rename) to prevent data loss on crash
- Manifest load-modify-save cycles are now protected by advisory file locking to prevent concurrent corruption
- `home/system packages add` saves the manifest after each successful install so partial failures leave the manifest in sync with the profile
- `home/system packages import` removes packages from the nix profile that are absent from the new manifest
- `env reload` resets the profile before rebuilding so packages removed from the manifest don't accumulate
- `build`, `log`, and `store gc` now return proper errors instead of calling `process::exit()` directly

- `completions` command generating shell completions for bash, zsh, fish, elvish, powershell
- `registry list/add/remove/pin/unpin/resolve` commands for managing flake registries
- `closure sbom-diff` command comparing closures by package with CVE, license, and provenance change tracking
- `closure sbom` command generating CycloneDX 1.7 SBOMs with CPE/PURL/license/source-distribution metadata
  - Enriches components via `nix eval --apply` with recursive dependency walk for full closure metadata
  - Multi-output packages coalesced into single components with aggregated size
  - Source distribution URLs from `src.urls`/`src.url` with binary distribution detection
  - `nix:position` and `nix:output_path` properties on each component
  - Embedded package manifest support for ekaos system closures
- `search packages/options/files` commands with cached ZSTD-compressed indexes
- `system switch/boot/test/build/rollback/list-generations` commands replacing nixos-rebuild
  - `system prune-boot-entries` command removing orphaned boot entries with optional `--gc` pass
  - `system packages add/remove/list/export/import` commands for imperative system package management
  - System packages managed via `~/.config/ekapkgs/system-packages.toml` manifest
  - Immediate install/remove via dedicated nix profile at `/nix/var/nix/profiles/ekapkgs-system-packages`
  - Export/import manifest for syncing system packages across machines
- `home switch/build/generations/packages` commands replacing home-manager
  - `home packages add/remove/list/export/import` commands for imperative package management
  - Packages managed via `~/.config/ekapkgs/home-packages.toml` manifest
  - Immediate install/remove via dedicated nix profile at `~/.ekapkgs-packages`
  - Export/import manifest for syncing packages across machines
- `env` command for directory-scoped package environments (direnv-style)
  - `env init/add/remove/list/reload/allow/disallow/hook` for manifest and trust management
  - `env flake-add/flake-remove/flake-pin` for composable multi-flake dev shells
  - Per-directory `.ekapkgs-env.toml` manifest with `[[packages]]` and `[[flakes]]` sections
  - Multiple flake dev shells composable into a single environment with input overrides
  - `flake-pin` locks flake entries to specific revisions (auto-resolves if omitted)
  - Shell hooks for bash, zsh, and fish that auto-activate/deactivate on `cd`
  - File watching: hooks detect changes to `flake.nix`, `flake.lock`, and the manifest, then auto-reload
  - Trust model: `env allow` / `env disallow` gate auto-activation (re-allow required after manifest edits)
  - Profiles cached at `~/.cache/ekapkgs/envs/<hash>/profile`
- `deploy` command with build, transfer, and activation lifecycle
- `develop` command with cache-aware pre-fetching for dev environments
- `flake show/metadata/update-diff` commands for flake introspection
  - `flake update-diff` shows closure size comparison before committing input updates
- `closure size/why-depends/diff` commands for closure analysis
- `log` command for viewing build logs
- `dry-run` command for build plan analysis with cache breakdown
- `store gc/optimize/verify` commands for nix store management
- `doctor` command checking nix, store, caches, and disk space
- `cache warm` command computing closure diffs between flake.lock versions for CI/CD pre-warming
- `cache push --sources-only` for low-bandwidth links transferring derivation graphs and FODs instead of built NARs
- `substituter` local proxy implementing nix binary cache protocol with batched narinfo queries
  - Critical path prioritization downloading target binary and runtime deps first for faster time-to-first-run
- Build progress monitor with live DAG display
- Resumable downloads with HTTP Range request support

### Server

- Threshold signing requiring k-of-n independent certificate signatures
- Prometheus metrics endpoint for server observability
- S3-compatible object storage backend
- Delta transfers using zstd dictionary compression between NAR versions
- gRPC NAR streaming for single-connection closure transfer
- Content-addressed storage backend with chunk-level deduplication
- Path traversal prevention and constant-time token comparison
  - Request body size limits and narinfo validation on push
  - Delta cache capped at 256 MiB to prevent unbounded memory growth

## 0.1.0

### Client

- `build/run/shell` commands wrapping nix with negotiated substitution
- `cache push/pull` commands for uploading and pre-fetching closures
  - `cache auth login/logout/status` commands for cache credential management
- Protobuf negotiation protocol resolving entire closures in a single gRPC round trip
- Certificate-based signing with CA keypair management for key rotation without client changes

### Server

- Binary cache server with nix-store and filesystem storage backends
  - LRU garbage collection for bounded filesystem cache
  - Server-side token management for push authentication
- Standard nix binary cache compatibility on the same port as gRPC

### Infrastructure

- CI pipeline with check, clippy, format, test, and nix build jobs
- Nix flake packaging and dev shell
