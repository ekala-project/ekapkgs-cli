# Testing

## Unit Tests

Inline in each crate. Run with:

```bash
cargo test --workspace
```

## Integration Tests

The `ekapkgs-integration-tests` crate (`crates/ekapkgs-integration-tests/`) spawns real server processes and validates both gRPC and HTTP endpoints. Tests use `tempfile` for isolated environments and generate signing keys and tokens on the fly.

```bash
cargo test -p ekapkgs-integration-tests
```
