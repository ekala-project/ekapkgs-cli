---
name: ekapkgs-fuse
description: Use when the user wants to mount a FUSE filesystem for shared libraries, serve /usr/lib from nix cache, or manage the ekapkgs FUSE mount.
---

# ekapkgs FUSE — On-Demand Shared Library Mount

Mount a FUSE filesystem at `/usr/lib` to serve shared libraries on-demand from the nix cache.

```bash
ekapkgs fuse mount [--mountpoint /usr/lib] [--foreground] [--upstream <url>]
ekapkgs fuse unmount [--mountpoint /usr/lib]
ekapkgs fuse status [--mountpoint /usr/lib]
```
