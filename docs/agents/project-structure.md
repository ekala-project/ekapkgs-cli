# Project Structure

```
crates/
  ekapkgs/                   # client binary — nix wrapper + cache push/pull
  ekapkgs-serve/             # server binary — gRPC + HTTP cache server
  ekapkgs-protocol/          # protobuf types + cert verification (no IO)
  ekapkgs-nix/               # nix CLI wrapping utilities
  ekapkgs-ui/                # logging, progress bars
  ekapkgs-integration-tests/ # integration test suite
proto/
  ekapkgs/v1/                # canonical .proto definitions
nix/                         # flake packaging and dev shell
plans/                       # feature roadmap documents
```

## Workspace Layout

Cargo workspace with 6 crates. All shared settings (edition, version, lints, dependencies) are defined in the root `Cargo.toml`.

- **Edition:** 2024
- **MSRV:** 1.85
- **License:** MPL-2.0
- **Resolver:** 3

## Key Crate Roles

| Crate | Purpose | Has IO? |
|---|---|---|
| `ekapkgs` | Client CLI — wraps nix commands, cache push/pull/auth, system/home management, search | Yes |
| `ekapkgs-serve` | Server — gRPC negotiation, HTTP compat, storage, tokens, GC | Yes |
| `ekapkgs-protocol` | Protobuf types, certificate verification | No |
| `ekapkgs-nix` | Nix command execution, eval, store path ops | Yes |
| `ekapkgs-ui` | Tracing setup, progress bars | No |
| `ekapkgs-integration-tests` | End-to-end tests spawning real server processes | Yes |
