# Changelog

## Unreleased

### Client

- `closure sbom-diff` command comparing closures by package with CVE, license, and provenance change tracking
- `closure sbom` command generating CycloneDX 1.5 SBOMs with embedded package manifest support for CPE/PURL/license metadata
- `search packages/options/files` commands with cached ZSTD-compressed indexes
- `system switch/boot/test/build/rollback/list-generations` commands replacing nixos-rebuild
  - `system prune-boot-entries` command removing orphaned boot entries with optional `--gc` pass
- `home switch/build/generations/packages` commands replacing home-manager
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
