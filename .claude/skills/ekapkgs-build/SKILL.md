---
name: ekapkgs-build
description: Use when the user wants to build a nix package, run a nix build, or compile something with ekapkgs. Covers ekapkgs build, run, shell, develop, and dry-run commands.
---

# ekapkgs Build Commands

## Build a package

```bash
ekapkgs build <installable> [extra args...]
# Example: ekapkgs build nixpkgs#hello
# Example: ekapkgs build .#myPackage --no-link
```

## Run a package

```bash
ekapkgs run <installable> [extra args...]
# Example: ekapkgs run nixpkgs#cowsay -- "hello"
```

## Enter a shell with packages

```bash
ekapkgs shell <installable>... [-- extra args]
# Example: ekapkgs shell nixpkgs#jq nixpkgs#ripgrep
```

## Development environment

```bash
ekapkgs develop [installable] [extra args...]
# Default installable is "." (current directory)
# Pre-fetches from configured cache before building
```

## Dry run (what would be built)

```bash
ekapkgs dryrun <installable> [extra args...]
# Shows what will be built vs substituted without building
```

## Try a package temporarily

```bash
ekapkgs try <packages>... [-- command args...]
# Example: ekapkgs try imagemagick -- convert in.png out.jpg
# Example: ekapkgs try jq ripgrep  # opens interactive shell
# --flake <flake>  override the default nixpkgs flake
```

## Build logs

```bash
ekapkgs log <installable>
# Show build log for a derivation
```

## Tips

- All build commands support cache pre-fetching when a cache is configured
- Use `--json` output where available for scripting
- The `try` command wraps `nix shell` -- nothing is persisted
- `wtf <command>` finds and runs a command you don't have installed
