# Command: `ekapkgs template`

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

## Features

- `--from-url` fetches metadata (description, license, version) from GitHub API and prefetches source hash via `nix-prefetch-url`
- `--stdout` prints to stdout instead of writing a file
- `--pname`, `--version`, `--description`, `--license` override metadata
- `auto` subcommand detects project type from indicator files (Cargo.toml, go.mod, pyproject.toml, meson.build, CMakeLists.txt, etc.)

## Code structure

The template command is implemented as a module directory at `commands/template/`:
- `mod.rs` — `execute()` dispatcher, URL metadata fetching, file output
- `types.rs` — `TemplateKind` enum, `Fetcher` enum, `ExpressionInfo` struct
- `expression.rs` — `generate()` function with per-template renderers, shared helpers for fetch blocks and meta blocks
- `detect.rs` — `detect_template()` scans directory for indicator files in priority order
- `url.rs` — `parse_url()` for GitHub/GitLab URL parsing, `fetch_github_metadata()` using reqwest (via inline tokio runtime)
- `prefetch.rs` — `prefetch_source_hash()` using `nix-prefetch-url --unpack` + `nix hash to-sri`
