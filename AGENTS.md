# Agent Guide for ekapkgs-cli

Nix CLI wrapper with a negotiated binary cache protocol. Two binaries: `ekapkgs` (client) and `ekapkgs-serve` (server). Resolves entire closures in a single gRPC round trip instead of ~3N HTTP requests.

## Project

- [Project Structure](docs/agents/project-structure.md) — workspace layout, crate roles, edition/MSRV
- [Build System & CI](docs/agents/build-and-ci.md) — prerequisites, cargo/nix commands, CI pipeline, validation checklist
- [Linting & Formatting](docs/agents/linting-and-formatting.md) — workspace lints, rustfmt, clippy config
- [Testing](docs/agents/testing.md) — unit tests, integration test suite

## Architecture

- [Protocol & Architecture](docs/agents/protocol.md) — negotiate RPC, signing models, client config
- [Server Storage Backends](docs/agents/server-storage.md) — filesystem, nix-store, castore (CAS), chunk negotiation, GC, HTTP endpoints

## Commands

- [system](docs/agents/cmd-system.md) — `nixos-rebuild` replacement; builds and activates system configurations with auto-rollback
- [home](docs/agents/cmd-home.md) — `home-manager` replacement; per-user dotfiles, packages, services with auto-rollback
- [search](docs/agents/cmd-search.md) — package, option, and file search using cached ZSTD-compressed indexes
- [closure sbom](docs/agents/cmd-closure-sbom.md) — CycloneDX/CSV SBOM generation from nix closures
- [env](docs/agents/cmd-env.md) — per-directory dev environments with automatic shell hook activation
- [template](docs/agents/cmd-template.md) — core-pkgs Nix expression generator (stdenv, cmake, meson, rust, go, python, auto)
- [registry](docs/agents/cmd-registry.md) — flake registry management (list, add, remove, pin, resolve)
