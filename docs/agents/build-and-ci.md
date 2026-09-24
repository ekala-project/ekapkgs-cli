# Build System & CI

## Prerequisites

Requires Rust 1.85+ and `protoc`. Use the Nix dev shell for a reproducible environment:

```bash
nix develop
```

Or manually:

```bash
nix shell nixpkgs#gcc nixpkgs#protobuf
```

## Common Commands

```bash
cargo build --workspace          # build everything
cargo test --workspace           # run all tests
cargo clippy --workspace -- -D warnings  # lint (CI treats warnings as errors)
cargo fmt --all -- --check       # check formatting
```

## Nix Builds

```bash
nix build .#ekapkgs              # client package
nix build .#ekapkgs-serve        # server package
```

## Protobuf

Proto files live in `proto/ekapkgs/v1/`. The `ekapkgs-protocol` crate compiles them via `tonic-build` in its `build.rs`. After modifying `.proto` files, `cargo build` regenerates the Rust types automatically.

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
- [ ] If adding a feature or command, update `CHANGELOG.md` under the appropriate section (Client/Server)
