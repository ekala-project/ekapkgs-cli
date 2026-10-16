---
name: ekapkgs-try
description: Use when the user wants to try a package without installing it, run a one-off command with a package, or use a package temporarily.
---

# ekapkgs Try — Ephemeral Package Use

```bash
# Interactive shell with packages available
ekapkgs try <packages>...
# Example: ekapkgs try jq ripgrep

# Run a single command
ekapkgs try <packages>... -- <command> <args>...
# Example: ekapkgs try imagemagick -- convert input.png output.jpg
# Example: ekapkgs try ffmpeg -- ffprobe video.mp4

# Use a different flake
ekapkgs try --flake github:user/repo my-tool -- my-tool --help
```

Nothing is persisted. Wraps `nix shell` with the correct flake reference. Package names are resolved against nixpkgs by default.
