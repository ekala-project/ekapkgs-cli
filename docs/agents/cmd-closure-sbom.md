# Command: `ekapkgs closure sbom`

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
