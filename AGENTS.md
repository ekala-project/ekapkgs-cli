# Agent Guide for ekapkgs-cli

Nix CLI wrapper with a negotiated binary cache protocol. Two binaries: `ekapkgs` (client) and `ekapkgs-serve` (server). Resolves entire closures in a single gRPC round trip instead of ~3N HTTP requests.

## Project Structure

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

### Workspace Layout

Cargo workspace with 6 crates. All shared settings (edition, version, lints, dependencies) are defined in the root `Cargo.toml`.

- **Edition:** 2024
- **MSRV:** 1.85
- **License:** MPL-2.0
- **Resolver:** 3

### Key Crate Roles

| Crate | Purpose | Has IO? |
|---|---|---|
| `ekapkgs` | Client CLI — wraps nix commands, cache push/pull/auth | Yes |
| `ekapkgs-serve` | Server — gRPC negotiation, HTTP compat, storage, tokens, GC | Yes |
| `ekapkgs-protocol` | Protobuf types, certificate verification | No |
| `ekapkgs-nix` | Nix command execution, eval, store path ops | Yes |
| `ekapkgs-ui` | Tracing setup, progress bars | No |
| `ekapkgs-integration-tests` | End-to-end tests spawning real server processes | Yes |

## Build System

### Prerequisites

Requires Rust 1.85+ and `protoc`. Use the Nix dev shell for a reproducible environment:

```bash
nix develop
```

Or manually:

```bash
nix shell nixpkgs#gcc nixpkgs#protobuf
```

### Common Commands

```bash
cargo build --workspace          # build everything
cargo test --workspace           # run all tests
cargo clippy --workspace -- -D warnings  # lint (CI treats warnings as errors)
cargo fmt --all -- --check       # check formatting
```

### Nix Builds

```bash
nix build .#ekapkgs              # client package
nix build .#ekapkgs-serve        # server package
```

### Protobuf

Proto files live in `proto/ekapkgs/v1/`. The `ekapkgs-protocol` crate compiles them via `tonic-build` in its `build.rs`. After modifying `.proto` files, `cargo build` regenerates the Rust types automatically.

## Linting & Formatting

### Rust Lints (workspace-wide)

- `unsafe_code = "forbid"` — no unsafe code allowed anywhere
- Clippy runs with `-D warnings` in CI — all warnings are errors
- Key clippy allows: `too_many_arguments`, `module_name_repetitions`
- Key clippy warns: `cloned_instead_of_copied`, `str_to_string`, `needless_pass_by_value`, `manual_let_else`, `match_same_arms`, `unnecessary_wraps`, `implicit_clone`, `inefficient_to_string`

### Rustfmt

Configured in `.rustfmt.toml`:
- Style edition 2024, max comment width 100
- Import grouping: Std, External, Crate
- Formats doc comments, macro bodies, strings

### Clippy

Configured in `clippy.toml`:
- `too_many_arguments` threshold: 8
- `enum_variant_size` threshold: 400
- `literal_representation` threshold: 8 (allows long hash literals)

## Testing

### Unit Tests

Inline in each crate. Run with:

```bash
cargo test --workspace
```

### Integration Tests

The `ekapkgs-integration-tests` crate (`crates/ekapkgs-integration-tests/`) spawns real server processes and validates both gRPC and HTTP endpoints. Tests use `tempfile` for isolated environments and generate signing keys and tokens on the fly.

```bash
cargo test -p ekapkgs-integration-tests
```

## CI Pipeline

Defined in `.github/workflows/ci.yml`. Five parallel jobs, all running in `nix develop`:

1. **Check** — `cargo check --workspace`
2. **Clippy** — `cargo clippy --workspace -- -D warnings`
3. **Format** — `cargo fmt --all -- --check`
4. **Test** — `cargo build --workspace` then `cargo test --workspace`
5. **Nix Build** — `nix build .#ekapkgs` and `nix build .#ekapkgs-serve`

Triggers on push to `master`/`main` and all pull requests.

## Validation Checklist

Before submitting changes:

- [ ] `cargo check --workspace` passes
- [ ] `cargo clippy --workspace -- -D warnings` passes (no warnings)
- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo test --workspace` passes
- [ ] If proto files changed, generated code builds correctly
- [ ] If adding dependencies, they are declared at workspace level in root `Cargo.toml`

## Architecture Notes

### Protocol

The core innovation is the negotiate RPC (`proto/ekapkgs/v1/negotiate.proto`):
- Client sends all wanted hashes + already-have hashes in one request
- Server responds with a topologically-sorted download plan (batches of paths with no mutual dependencies)
- Supports compression preferences and certificate-based trust

### Server Storage Backends

- **filesystem** — cache directory with `{hash}.narinfo` + `nar/` files, LRU garbage collection
- **nix-store** — serves directly from `/nix/store` via the nix daemon

### Signing

Two models supported:
- **Standard nix signing** — Ed25519 secret key signs narinfo fingerprints
- **Certificate-based signing** — CA keypair issues short-lived certificates for key rotation without client config changes

### Client Configuration

Config at `~/.config/ekapkgs/config.toml`. Supports multiple caches with priorities and per-cache tokens.
