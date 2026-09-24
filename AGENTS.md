# Agent Guide for ekapkgs-cli

Nix CLI wrapper with a negotiated binary cache protocol. Two binaries: `ekapkgs` (client) and `ekapkgs-serve` (server). Resolves entire closures in a single gRPC round trip instead of ~3N HTTP requests. Also provides `system` (nixos-rebuild replacement), `home` (home-manager replacement), `search` (package/option/file search), `closure sbom` (SBOM generation), `registry` (flake registry management), and `template` (core-pkgs package expression generation) commands.

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
| `ekapkgs` | Client CLI — wraps nix commands, cache push/pull/auth, system/home management, search | Yes |
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
- [ ] If adding a feature or command, update `CHANGELOG.md` under the appropriate section (Client/Server)

## Architecture Notes

### Protocol

The core innovation is the negotiate RPC (`proto/ekapkgs/v1/negotiate.proto`):
- Client sends all wanted hashes + already-have hashes in one request
- Server responds with a topologically-sorted download plan (batches of paths with no mutual dependencies)
- Supports compression preferences and certificate-based trust
- CAS-aware clients can use `NegotiateChunks` RPC for chunk-level negotiation (only transfer missing chunks)

### Server Storage Backends

- **filesystem** — cache directory with `{hash}.narinfo` + `nar/` files, LRU garbage collection
- **nix-store** — serves directly from `/nix/store` via the nix daemon
- **castore** — content-addressed chunk store with file-level deduplication (see below)

### Signing

Two models supported:
- **Standard nix signing** — Ed25519 secret key signs narinfo fingerprints
- **Certificate-based signing** — CA keypair issues short-lived certificates for key rotation without client config changes

### Content-Addressed Store (`castore` backend)

The `castore` storage backend decomposes NARs into a content-addressed Merkle tree of blake3-hashed chunks, enabling file-level deduplication across store paths. Two packages sharing identical files store those files' chunks only once.

#### On-disk layout

```
{root}/
  chunks/{hex[0..4]}/{hex}.chunk   — FastCDC blob chunks (16K-256K, 64K avg)
  castore.db                       — SQLite metadata (WAL mode)
```

#### SQLite schema (castore.db)

- `cas_paths` — maps store path hash → root CaNode (protobuf), narinfo metadata, `last_access` for GC
- `chunks` — chunk digest → size
- `file_chunks` — file digest → ordered list of (chunk_digest, chunk_size) pairs
- `directories` — directory digest → serialized CaDirectory protobuf

#### Ingest flow (`put_nar`)

1. Parse incoming NAR into a `NarNode` tree (`ekapkgs-nix/src/nar.rs`)
2. Recursively walk the tree (`ingest_node`):
   - **Files**: FastCDC chunk the content, write chunk files to disk (idempotent), record chunks and file-chunk mappings in DB
   - **Directories**: Serialize as `CaDirectory` protobuf, store in SQLite with blake3 digest
   - **Symlinks**: Encoded directly in the CaNode tree
3. Store root CaNode + narinfo metadata in `cas_paths`
4. All DB writes wrapped in a single SQLite transaction for crash safety

#### Retrieval flow (`get_nar`)

1. Load root CaNode from `cas_paths`
2. Recursively reconstruct `NarNode` tree from CAS data (load directories from SQLite, concatenate file chunks from disk)
3. Directories verified against expected blake3 digest on load
4. Serialize back to NAR bytes via `write_nar()`

#### Chunk-level negotiation (`NegotiateChunks` RPC)

Server-side (`api/negotiate.rs`): walks requested paths' Merkle trees and returns root nodes, directory data, file-chunk mappings, and only the chunks the client is missing. Request size capped (10K want, 10K have, 500K have_chunks).

Client-side (`cas_pull.rs`, `chunk_store.rs`): maintains a local chunk store at `~/.cache/ekapkgs/castore/` with SQLite metadata. Downloads missing chunks in parallel with blake3 verification, reassembles NARs, verifies sha256 against narinfo hash, then imports to nix store. Falls back to NAR streaming on failure.

#### 3-tier pull fallback

The client pull command (`commands/cache.rs`) tries in order:
1. **CAS chunks** — if server has CAS data, negotiate at chunk level
2. **gRPC streaming** — stream full NARs over `StreamNars` RPC
3. **HTTP batch** — individual HTTP GET per NAR

Each tier falls back to the next on failure or if the server doesn't support it.

#### GC support

- `evict_path()` walks the Merkle tree, deletes the path, removes orphaned file_chunks and chunks (computed reference checks, not tracked ref_count)
- `update_access()` for batch last-access updates
- `paths_by_access_asc()` for LRU eviction ordering
- Shared chunks (referenced by multiple paths) are preserved until all referencing paths are evicted

#### HTTP endpoints

- `GET/PUT /cas/chunk/{b3hex}` — individual chunk download/upload with blake3 verification
- `GET /nar/{hash}.nar` — transparently reconstructs NAR from CAS chunks

#### Protobuf types (`proto/ekapkgs/v1/castore.proto`)

- `CaNode` — oneof: `CaDirectoryNode`, `CaFileNode`, `CaSymlinkNode`
- `CaDirectory` — repeated `CaDirectoryEntry` (name + CaNode)
- `B3Digest` — 32-byte blake3 digest
- `ChunkMeta` — digest + size
- `ChunkNegotiateRequest/Response` — chunk-level negotiation messages
- `CaPathMapping` — store path hash → root CaNode
- `FileChunkMapping` — file digest → ordered chunk list

### System Management (`ekapkgs system`)

Replaces `nixos-rebuild` for local system configuration. Builds `system.build.toplevel` from the flake, manages `/nix/var/nix/profiles/system`, and activates via `switch-to-configuration`. Subcommands: `switch`, `boot`, `test`, `build`, `list-generations`, `rollback`, `prune-boot-entries`.

- `switch` auto-rolls back to the previous profile if activation fails
- `prune-boot-entries` removes orphaned BLS entries, kernel/initrd files, and UKI files from the ESP after generations are garbage collected
- `--gc` flag on `prune-boot-entries` runs `nix-collect-garbage -d` first

### Home Configuration (`ekapkgs home`)

Replaces `home-manager`. Per-user dotfiles, packages, environment variables, shell aliases, and activation scripts are defined in the ekaos module system under `users.users.<name>` and built as `system.build.home`. The activation script runs as the user (no root) and manages symlinks into `$HOME` with a JSON manifest for cleanup. State stored at `~/.config/ekaos/`. Subcommands: `switch`, `build`, `generations`, `rollback`, `packages`, `services`.

- `switch` auto-rolls back to the previous generation if activation fails
- `rollback` activates the previous home generation

Related ekaos module: `modules/config/home.nix` in the `core-pkgs` repo.

### Search (`ekapkgs search`)

Searches packages, configuration options, or files using ZSTD-compressed JSON indexes cached at `~/.cache/ekapkgs/indexes/`. Indexes auto-generate on first use via nix evaluation, or can be downloaded from a remote URL. File search integrates with `nix-locate` when available.

Related ekaos file: `lib/generate-options-index.nix` for option index generation.

### SBOM Generation (`ekapkgs closure sbom`)

Generates CycloneDX 1.7 JSON or CSV Software Bill of Materials from a nix closure. Extracts package metadata (CPE, PURL, license, description, source URLs, position) via `nix eval --apply` with a recursive dependency walk through `buildInputs`/`propagatedBuildInputs`. Three-tier metadata fallback: embedded package manifest > eval metadata > store-path heuristic.

- Default: runtime-only closure (avoids bootstrap/build-tool noise)
- `--buildtime` flag includes full build closure
- `--format cyclonedx` (default) or `--format csv`
- `-o FILE` to write to file instead of stdout
- Multi-output packages coalesced into single components with aggregated NAR size
- Source distribution URLs from `src.urls`/`src.url` as `externalReferences`
- `nix:position`, `nix:output_path`, `nix:mainProgram` properties per component
- Component type set to `application` when `meta.mainProgram` is defined
- Dependency graph derived from `nix path-info` references

Related ekaos module: `modules/system/package-manifest.nix` in the `core-pkgs` repo generates the embedded manifest with role classification (`default`, `user`, `service`, `home`, `boot`).

### Directory Environments (`ekapkgs env`)

Per-directory package and dev shell environments with automatic shell hook activation. Manifests are `.ekapkgs-env.toml` files in the project root. Two activation modes:

**Packages-only mode** (manifest has only `[[packages]]`): The shell hook prepends the nix profile's `bin/` to PATH. Lightweight, no child shell.

**Dev shell mode** (manifest has `[[flakes]]`): The shell hook spawns a child shell with the full dev environment pre-loaded. The environment is rendered via `nix print-dev-env --json` and cached as sourceable scripts. Entering the directory pushes the child shell; leaving exits it, returning to the parent.

#### Key files and state

- Manifest: `.ekapkgs-env.toml` (per-directory, committed to repo)
- Profile: `~/.cache/ekapkgs/envs/{blake3(dir)[..32]}/profile` (nix profile with packages/flake outputs)
- Rendered env scripts: `~/.cache/ekapkgs/envs/{hash}/env.{bash,zsh,fish}` (sourceable dev shell scripts)
- Fingerprint: `~/.cache/ekapkgs/envs/{hash}/env.fingerprint` (for staleness checks)
- Trust database: `~/.config/ekapkgs/trusted-envs.toml` (maps canonical dir path → manifest content hash)

#### Dev shell rendering pipeline

`render_dev_env()` in `commands/env.rs`:
1. For each `[[flakes]]` entry, runs `nix print-dev-env --json {flake}#devShells.{system}.{devshell}`
2. Parses `PrintDevEnvOutput` (variables + bashFunctions)
3. Merges results from multiple flakes (PATH concatenated, other vars last-wins, functions unioned)
4. Filters out skip-listed variables (session, bash-internal, nix-build-internal — `SKIP_VARIABLES` const)
5. Writes shell-specific scripts (`env.bash`, `env.zsh`, `env.fish`) and `env.fingerprint`
6. Triggered by `ekapkgs env reload` or lazily by the shell hook on first activation

#### Shell hook architecture

Three embedded shell hooks (bash, zsh, fish) in `commands/env.rs`. Key functions:

- `_ekapkgs_env_hook()` — runs on every prompt/chpwd; walks up directory tree looking for `.ekapkgs-env.toml`; guarded by `_EKAPKGS_ENV_CHILD`, `_EKAPKGS_ENV_SPAWNING`, and `_EKAPKGS_ENV_COOLDOWN` to prevent recursion and re-spawn loops
- `_ekapkgs_env_activate()` — checks trust, probes for cached dev shell (`_has-devshell`), spawns child shell or falls back to PATH-only; lazy-renders on first activation
- `_ekapkgs_env_spawn_child()` — spawns a child shell (bash: `--rcfile`, zsh: `ZDOTDIR` tempdir, fish: `--init-command`) that sources the pre-rendered env script and monitors `cd` via prompt hook
- `_ekapkgs_env_deactivate()` — restores PATH from backup (packages-only mode)

Hidden internal commands used by hooks: `_profile-bin`, `_is-trusted`, `_fingerprint`, `_reload`, `_has-devshell`, `_render-env`.

#### Trust and change detection

- `env allow` stores `blake3(manifest contents)` — any manifest edit invalidates trust
- `compute_fingerprint()` hashes mtimes of `.ekapkgs-env.toml`, `flake.nix`, `flake.lock` — cheap per-prompt staleness check
- Inside a child shell, fingerprint changes trigger `_render-env` + re-source (live reload without exiting)

### Package Templates (`ekapkgs template`)

Generates core-pkgs-compatible Nix package expressions. Subcommands: `stdenv`, `cmake`, `meson`, `rust`, `go`, `python`, `auto`.

Each template produces a complete `callPackage`-compatible expression following core-pkgs conventions:
- Always uses `finalAttrs:` pattern (e.g., `stdenv.mkDerivation (finalAttrs: { ... })`)
- Uses `tag` instead of `rev` in fetchers (e.g., `tag = "v${finalAttrs.version}";`)
- No `maintainers` in meta — core-pkgs does not use `meta.maintainers`
- CMake templates include `cmake.configurePhaseHook` in `nativeBuildInputs` and use `cmakeEntries` (structured attr-set)
- Meson templates include `meson.configurePhaseHook` and `ninja` in `nativeBuildInputs` and use `mesonEntries`
- Python templates use `pyproject = true`, `build-system`, and `pythonImportsCheck`
- Rust templates use `cargoHash` (defaults to placeholder hash)
- Go templates use `vendorHash` and include `mainProgram`

Features:
- `--from-url` fetches metadata (description, license, version) from GitHub API and prefetches source hash via `nix-prefetch-url`
- `--stdout` prints to stdout instead of writing a file
- `--pname`, `--version`, `--description`, `--license` override metadata
- `auto` subcommand detects project type from indicator files (Cargo.toml, go.mod, pyproject.toml, meson.build, CMakeLists.txt, etc.)

#### Code structure

The template command is implemented as a module directory at `commands/template/`:
- `mod.rs` — `execute()` dispatcher, URL metadata fetching, file output
- `types.rs` — `TemplateKind` enum, `Fetcher` enum, `ExpressionInfo` struct
- `expression.rs` — `generate()` function with per-template renderers, shared helpers for fetch blocks and meta blocks
- `detect.rs` — `detect_template()` scans directory for indicator files in priority order
- `url.rs` — `parse_url()` for GitHub/GitLab URL parsing, `fetch_github_metadata()` using reqwest (via inline tokio runtime)
- `prefetch.rs` — `prefetch_source_hash()` using `nix-prefetch-url --unpack` + `nix hash to-sri`

### Flake Registry (`ekapkgs registry`)

Wraps `nix registry` subcommands for managing flake registries. Registries map symbolic flake identifiers (e.g., `nixpkgs`) to full URLs (e.g., `github:NixOS/nixpkgs`). Subcommands: `list`, `add`, `remove`, `pin`, `resolve`.

- `add` and `remove` support `--registry` to operate on a specific registry file
- `pin` locks a registry entry to a specific revision
- `resolve` translates indirect flake references to direct URLs

### Client Configuration

Config at `~/.config/ekapkgs/config.toml`. Supports multiple caches with priorities and per-cache tokens.

All TOML manifest writes (home-packages, system-packages, home-services, env manifests, trusted-envs) use atomic write-then-rename via `tempfile::NamedTempFile`. Mutation commands hold an advisory file lock (`fs2::lock_exclusive`) for the entire load-modify-save window to prevent concurrent corruption.
