---
name: ekapkgs-closure
description: Use when the user wants to analyze nix closures, check closure sizes, generate SBOMs, diff closures, trace dependencies, or create Docker images from nix packages.
---

# ekapkgs Closure Analysis

## Closure size breakdown

```bash
ekapkgs closure size <installable>
# Example: ekapkgs closure size nixpkgs#hello
# Shows all paths in the runtime closure sorted by NAR size
```

## Trace dependency chains

```bash
ekapkgs closure why-depends <installable> <dependency>
# Example: ekapkgs closure why-depends .#system glibc
```

## Diff two closures

```bash
ekapkgs closure diff <a> <b>
# When both are .drv paths: recursive derivation diff (like nix-diff)
# Otherwise: delegates to nix store diff-closures
# --depth <n>  max recursion depth for drv diffs
```

## Generate SBOM

```bash
ekapkgs closure sbom <installable>
# --format cyclonedx|csv  (default: cyclonedx)
# --buildtime  include build-time dependencies
# -o <file>  output file (default: stdout)
# Produces CycloneDX 1.7 JSON with dependency relationships
```

## Diff closures as package changes

```bash
ekapkgs closure sbom-diff <old> <new>
# --format text|json|csv  (default: text)
# -o <file>  output file
# Shows added, removed, and changed packages between two closures
```

## Create Docker image from closure

```bash
ekapkgs closure docker <installable>
# --name <name>  Docker image name
# --tag <tag>  image tag (default: latest)
# --entrypoint <cmd>  container entrypoint
# --cmd <args>...  default command
# -o <file>  write tarball instead of loading into Docker
```
