# Protocol & Architecture

## Negotiate RPC

The core innovation is the negotiate RPC (`proto/ekapkgs/v1/negotiate.proto`):
- Client sends all wanted hashes + already-have hashes in one request
- Server responds with a topologically-sorted download plan (batches of paths with no mutual dependencies)
- Supports compression preferences and certificate-based trust
- CAS-aware clients can use `NegotiateChunks` RPC for chunk-level negotiation (only transfer missing chunks)

## Signing

Two models supported:
- **Standard nix signing** — Ed25519 secret key signs narinfo fingerprints
- **Certificate-based signing** — CA keypair issues short-lived certificates for key rotation without client config changes

## Client Configuration

Config at `~/.config/ekapkgs/config.toml`. Supports multiple caches with priorities and per-cache tokens.

All TOML manifest writes (home-packages, system-packages, home-services, env manifests, trusted-envs) use atomic write-then-rename via `tempfile::NamedTempFile`. Mutation commands hold an advisory file lock (`fs2::lock_exclusive`) for the entire load-modify-save window to prevent concurrent corruption.
