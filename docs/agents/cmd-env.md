# Command: `ekapkgs env`

Per-directory package and dev shell environments with automatic shell hook activation. Manifests are `.ekapkgs-env.toml` files in the project root. Two activation modes:

**Packages-only mode** (manifest has only `[[packages]]`): The shell hook prepends the nix profile's `bin/` to PATH. Lightweight, no child shell.

**Dev shell mode** (manifest has `[[flakes]]`): The shell hook spawns a child shell with the full dev environment pre-loaded. The environment is rendered via `nix print-dev-env --json` and cached as sourceable scripts. Entering the directory pushes the child shell; leaving exits it, returning to the parent.

## Key files and state

- Manifest: `.ekapkgs-env.toml` (per-directory, committed to repo)
- Profile: `~/.cache/ekapkgs/envs/{blake3(dir)[..32]}/profile` (nix profile with packages/flake outputs)
- Rendered env scripts: `~/.cache/ekapkgs/envs/{hash}/env.{bash,zsh,fish}` (sourceable dev shell scripts)
- Fingerprint: `~/.cache/ekapkgs/envs/{hash}/env.fingerprint` (for staleness checks)
- Trust database: `~/.config/ekapkgs/trusted-envs.toml` (maps canonical dir path → manifest content hash)

## Dev shell rendering pipeline

`render_dev_env()` in `commands/env.rs`:
1. For each `[[flakes]]` entry, runs `nix print-dev-env --json {flake}#devShells.{system}.{devshell}`
2. Parses `PrintDevEnvOutput` (variables + bashFunctions)
3. Merges results from multiple flakes (PATH concatenated, other vars last-wins, functions unioned)
4. Filters out skip-listed variables (session, bash-internal, nix-build-internal — `SKIP_VARIABLES` const)
5. Writes shell-specific scripts (`env.bash`, `env.zsh`, `env.fish`) and `env.fingerprint`
6. Triggered by `ekapkgs env reload` or lazily by the shell hook on first activation

## Shell hook architecture

Three embedded shell hooks (bash, zsh, fish) in `commands/env.rs`. Key functions:

- `_ekapkgs_env_hook()` — runs on every prompt/chpwd; walks up directory tree looking for `.ekapkgs-env.toml`; guarded by `_EKAPKGS_ENV_CHILD`, `_EKAPKGS_ENV_SPAWNING`, and `_EKAPKGS_ENV_COOLDOWN` to prevent recursion and re-spawn loops
- `_ekapkgs_env_activate()` — checks trust, probes for cached dev shell (`_has-devshell`), spawns child shell or falls back to PATH-only; lazy-renders on first activation
- `_ekapkgs_env_spawn_child()` — spawns a child shell (bash: `--rcfile`, zsh: `ZDOTDIR` tempdir, fish: `--init-command`) that sources the pre-rendered env script and monitors `cd` via prompt hook
- `_ekapkgs_env_deactivate()` — restores PATH from backup (packages-only mode)

Hidden internal commands used by hooks: `_profile-bin`, `_is-trusted`, `_fingerprint`, `_reload`, `_has-devshell`, `_render-env`.

## Trust and change detection

- `env allow` stores `blake3(manifest contents)` — any manifest edit invalidates trust
- `compute_fingerprint()` hashes mtimes of `.ekapkgs-env.toml`, `flake.nix`, `flake.lock` — cheap per-prompt staleness check
- Inside a child shell, fingerprint changes trigger `_render-env` + re-source (live reload without exiting)
