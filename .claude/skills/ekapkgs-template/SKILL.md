---
name: ekapkgs-template
description: Use when the user wants to generate a Nix package expression, create a default.nix, scaffold a nix package, or auto-detect project type for packaging.
---

# ekapkgs Template — Nix Expression Generation

Generate core-pkgs Nix package expressions:

```bash
ekapkgs template stdenv [path]   # autotools/make
ekapkgs template cmake [path]    # CMake
ekapkgs template meson [path]    # Meson
ekapkgs template rust [path]     # buildRustPackage
ekapkgs template go [path]       # buildGoModule
ekapkgs template python [path]   # buildPythonPackage
ekapkgs template auto [path]     # Auto-detect from source files
```

## Common flags

```
--pname <name>          Package name
--version <ver>         Package version
--description <desc>    meta.description
--from-url <url>        Fetch source from GitHub/GitLab URL
--license <license>     License (nixpkgs short name: mit, gpl3Plus, etc.)
--stdout                Print to stdout instead of writing file
```

Default output path is `default.nix`.
