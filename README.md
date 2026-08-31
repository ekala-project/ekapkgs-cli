# ekapkgs-cli

Nix CLI wrapper with a negotiated binary cache protocol. Resolves entire
closures in a single round trip instead of one HTTP request per store path.

Two binaries: `ekapkgs` (client) and `ekapkgs-serve` (server).

## Why

The standard nix binary cache protocol requires ~3N HTTP requests for an
N-path closure (HEAD + GET narinfo + GET NAR, per path). `ekapkgs` sends
the full set of needed hashes to the server in one gRPC call and gets back
a manifest of everything available, with a topologically-sorted download plan.

The server also supports certificate-based signing (key rotation without
client config changes) and serves as a drop-in nix binary cache for
backward compatibility.

## Client

### Build, run, shell

```
ekapkgs build nixpkgs#hello
ekapkgs run nixpkgs#hello
ekapkgs shell nixpkgs#hello nixpkgs#jq
ekapkgs develop                         # cache-aware nix develop
ekapkgs develop .#devShells.x86_64-linux.default
```

### Cache management

```
ekapkgs cache push nixpkgs#hello           # upload closure to cache
ekapkgs cache pull nixpkgs#firefox          # pre-fetch closure
ekapkgs cache auth login URL --token TOKEN  # save push credentials
ekapkgs cache auth status                   # show configured caches
```

### Closure analysis

```
ekapkgs closure size nixpkgs#hello          # size breakdown by path
ekapkgs closure why-depends nixpkgs#hello nixpkgs#glibc
ekapkgs closure diff nixpkgs#hello nixpkgs#curl
ekapkgs closure sbom nixpkgs#hello          # CycloneDX 1.7 SBOM (runtime closure)
ekapkgs closure sbom nixpkgs#hello --buildtime        # include build-time deps
ekapkgs closure sbom nixpkgs#hello --format csv       # CSV output
ekapkgs closure sbom nixpkgs#hello -o sbom.cdx.json   # write to file
ekapkgs closure sbom-diff OLD NEW                      # diff two closures by package
ekapkgs closure sbom-diff OLD NEW --format json        # structured JSON diff
ekapkgs closure sbom-diff OLD NEW --format csv         # CSV diff
```

Package metadata (CPE, PURL, license, description, source URLs, position)
is extracted via `nix eval --apply` with recursive dependency traversal,
enriching the full closure. Multi-output packages are coalesced into single
components. Each component includes `nix:output_path` and `nix:position`
properties, source distribution external references, and is typed as
`application` when `meta.mainProgram` is defined. For ekaos system closures,
additional metadata (role, provenance) comes from the embedded package
manifest.

`sbom-diff` compares closures by package name and reports version changes,
added/removed packages, and metadata changes including new or resolved CVEs,
license changes, and source provenance shifts. Each entry includes a PURL
identifier (when available) for downstream tooling correlation.

### Build log and dry run

```
ekapkgs log nixpkgs#hello                  # show build log
ekapkgs dry-run nixpkgs#hello              # build plan with cache breakdown
```

### Store management

```
ekapkgs store gc                           # garbage collect
ekapkgs store gc --older-than 30d          # delete paths older than 30 days
ekapkgs store gc --dry-run                 # preview what would be deleted
ekapkgs store optimize                     # deduplicate via hardlinks
ekapkgs store verify --all                 # check store integrity
ekapkgs store verify --all --repair        # repair invalid paths
```

### Flake introspection

```
ekapkgs flake show                         # colored output tree
ekapkgs flake metadata                     # input dependency tree with revisions
ekapkgs flake update-diff nixpkgs          # show closure diff before committing update
```

### Flake registry

Manage flake registries (symbolic identifiers like `nixpkgs` that map to
full flake URLs):

```
ekapkgs registry list                      # show all registry entries
ekapkgs registry add nixpkgs github:NixOS/nixpkgs  # add/replace entry
ekapkgs registry remove nixpkgs            # remove entry
ekapkgs registry pin nixpkgs               # pin to current revision
ekapkgs registry unpin nixpkgs             # remove pin (restore floating)
ekapkgs registry resolve nixpkgs           # show resolved URL
```

### System management

Replaces `nixos-rebuild` for local system configuration:

```
ekapkgs system switch                      # build and activate
ekapkgs system boot                        # add boot entry, activate on reboot
ekapkgs system test                        # activate without updating boot entry
ekapkgs system build                       # build only, print store path
ekapkgs system list-generations            # list system generations
ekapkgs system rollback                    # roll back to previous generation
ekapkgs system rollback --dry-run          # preview rollback
ekapkgs system prune-boot-entries          # remove orphaned boot entries from ESP
ekapkgs system prune-boot-entries --gc     # garbage collect first, then prune
ekapkgs system prune-boot-entries --dry-run
```

#### Imperative system packages

Add packages to the system imperatively. Managed via
`~/.config/ekapkgs/system-packages.toml` with immediate install (via sudo)
into a dedicated nix profile at `/nix/var/nix/profiles/ekapkgs-system-packages`:

```
ekapkgs system packages add htop vim         # add and install (requires sudo)
ekapkgs system packages remove htop          # remove from manifest and profile
ekapkgs system packages list                 # list installed packages
ekapkgs system packages export -o sys.toml   # export manifest
ekapkgs system packages import sys.toml      # import and install
```

### Home configuration

Replaces `home-manager` for per-user dotfiles, packages, and environment:

```
ekapkgs home switch                        # build and activate home config
ekapkgs home build                         # build only, print store path
ekapkgs home generations                   # list home generations
```

Home configuration is defined in the ekaos module system under
`users.users.<name>` and built via `system.build.home`.

#### Imperative home packages

Add packages to your home without editing nix configuration. Managed via
`~/.config/ekapkgs/home-packages.toml` with immediate install into a
dedicated nix profile at `~/.ekapkgs-packages`:

```
ekapkgs home packages add alacritty ripgrep  # add and install immediately
ekapkgs home packages remove alacritty       # remove from manifest and profile
ekapkgs home packages list                   # list installed packages
ekapkgs home packages list --json            # machine-readable output
ekapkgs home packages export                 # print manifest to stdout
ekapkgs home packages export -o pkgs.toml    # export to file
ekapkgs home packages import pkgs.toml       # import and install
ekapkgs home packages import pkgs.toml --merge  # merge with existing
```

Packages can come from different flakes:

```
ekapkgs home packages add my-tool --flake github:user/repo
```

### Directory environments

Per-directory package environments with automatic activation, similar to
direnv. Each directory can have a `.ekapkgs-env.toml` manifest specifying
packages and flake dev shells:

```
ekapkgs env init                           # create .ekapkgs-env.toml
ekapkgs env add jq ripgrep fd              # add packages
ekapkgs env remove fd                      # remove a package
ekapkgs env list                           # show packages and flakes
ekapkgs env reload                         # rebuild profile from manifest
ekapkgs env allow                          # trust this environment
ekapkgs env disallow                       # revoke trust
```

#### Composable flake dev shells

Multiple flakes can be composed into a single environment. Each can
specify a dev shell attribute, pinned revision, and input overrides:

```
ekapkgs env flake-add .                    # add local flake
ekapkgs env flake-add github:user/repo --devshell python
ekapkgs env flake-add github:other/tools --rev abc123
ekapkgs env flake-add github:foo/bar --override-input nixpkgs=github:NixOS/nixpkgs/nixos-24.05
ekapkgs env flake-remove github:user/repo
ekapkgs env flake-pin github:other/tools   # pin to current revision
ekapkgs env flake-pin github:other/tools --rev deadbeef
```

#### Shell hooks

Add to your shell configuration for automatic activation on `cd`:

```bash
# ~/.bashrc
eval "$(ekapkgs env hook bash)"

# ~/.zshrc
eval "$(ekapkgs env hook zsh)"

# ~/.config/fish/config.fish
ekapkgs env hook fish | source
```

The hook automatically activates when you enter a directory with a trusted
`.ekapkgs-env.toml`, deactivates when you leave, and reloads when
`flake.nix`, `flake.lock`, or the manifest changes.

#### Manifest format

```toml
version = 1
flake = "nixpkgs"

[[packages]]
name = "jq"

[[packages]]
name = "my-tool"
flake = "github:user/repo"

[[flakes]]
ref_ = "."
devshell = "default"

[[flakes]]
ref_ = "github:other/tools"
devshell = "default"
rev = "abc123def456"

[flakes.inputs]
nixpkgs = "github:NixOS/nixpkgs/nixos-24.05"
```

### Search

Search packages, configuration options, or files. Indexes are cached
locally as ZSTD-compressed JSON and auto-generated on first use.

```
ekapkgs search packages hello              # search by name/description
ekapkgs search packages hello --json       # machine-readable output
ekapkgs search options boot.loader         # search configuration options
ekapkgs search files bin/hello             # find which package provides a file
ekapkgs search update                      # regenerate indexes
ekapkgs search update --remote https://...  # download pre-built indexes
```

### Remote deployment

```
ekapkgs deploy .#nixosConfigurations.prod --target-host prod-server
ekapkgs deploy .#nixosConfigurations.prod --target-host prod-server --mode boot
ekapkgs deploy .#nixosConfigurations.prod --target-host prod-server --build-host builder
ekapkgs deploy .#nixosConfigurations.prod --target-host prod-server --dry-run
```

### System diagnostics

```
ekapkgs doctor                             # check nix, store, caches, disk space
```

### Shell completions

```
ekapkgs completions bash > ~/.bash_completion.d/ekapkgs
ekapkgs completions zsh > ~/.zsh/completions/_ekapkgs
ekapkgs completions fish > ~/.config/fish/completions/ekapkgs.fish
```

### Configuration

Config at `~/.config/ekapkgs/config.toml`:

```toml
[[caches]]
url = "https://cache.ekapkgs.org"
token = "ekap_..."
priority = 10
```

## Server

```
# Quick start — serve from local nix store
ekapkgs-serve --signing-key cache-key.sec --storage nix-store

# With config file
ekapkgs-serve --config /etc/ekapkgs-serve/config.toml

# Token management
ekapkgs-serve token create ci-main          # prints token
ekapkgs-serve token create ci-pr --read-only
ekapkgs-serve token list
ekapkgs-serve token revoke ci-main

# Certificate signing (optional)
ekapkgs-serve generate-ca ekapkgs-root-ca-1
ekapkgs-serve issue-cert cache-2025 --ca-key ekapkgs-root-ca-1.sec --ca-name ekapkgs-root-ca-1
```

Server config:

```toml
[server]
bind = "0.0.0.0:8080"

[storage]
backend = "filesystem"  # or "nix-store"
path = "/var/cache/ekapkgs"

[storage.gc]
max_size = "50GiB"
gc_interval_secs = 300

[signing]
secret_key_file = "/etc/ekapkgs-serve/cache-key.sec"

[signing.certificate]
cert_file = "/etc/ekapkgs-serve/cache-2025.cert.json"
private_key_file = "/etc/ekapkgs-serve/cache-2025.key"

[auth]
write_tokens = ["legacy-token-if-needed"]
```

### Storage backends

- **filesystem** — reads/writes a cache directory (`{hash}.narinfo` + `nar/`).
  Supports LRU garbage collection with configurable size limits.
- **nix-store** — serves directly from `/nix/store` via the nix daemon,
  like `nix-serve`. No cache directory needed.

### Nix compatibility

The server speaks the standard nix binary cache protocol on the same port
as gRPC. Plain `nix build --substituters http://your-server` works unchanged.

## Building

Requires Rust 1.85+ and `protoc`:

```
nix shell nixpkgs#gcc nixpkgs#protobuf
cargo build --workspace
cargo test --workspace
```

## Project structure

```
crates/
  ekapkgs/            # client binary
  ekapkgs-serve/      # server binary
  ekapkgs-protocol/   # protobuf types + cert verification (no IO)
  ekapkgs-nix/        # nix CLI wrapping
  ekapkgs-ui/         # logging, progress bars
proto/
  ekapkgs/v1/         # canonical .proto definitions
```

## License

MPL-2.0
