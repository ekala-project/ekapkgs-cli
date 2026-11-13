# iets

[iets](https://github.com/numtide/iets) is a Rust reimplementation of the Nix
evaluator and the `nix` commands that evaluate. ekapkgs runs it as a
subprocess. It does not link the library, which never frees memory and is
LGPL-2.1+.

## Routing

Every `NixCommand` asks `ekapkgs_nix::iets::route` which program to run
(`crates/ekapkgs-nix/src/iets.rs`). A command runs as `iets nix ARGS...` only
when iets is installed and evaluates the command itself. Everything else runs
as plain `nix ARGS...`.

`iets nix` is iets's classic mode. It produces the same stdout, JSON and exit
codes as `nix`, and `--log-format internal-json` emits the same `@nix`
records, so `BuildMonitor` works unchanged. For anything it does not
implement, it prints `iets: falling back to nix: REASON` and execs the real
`nix`. The routing checks only avoid that extra exec and notice. iets still
falls back on its own for cases they miss, for example a `builtins.getFlake`
on a registry ref inside `--expr`.

A command goes to iets when all of these hold:

- The subcommand is `eval`, `build`, `derivation show`, `search`,
  `print-dev-env`, `develop`, `fmt` or `flake prefetch-inputs`.
- No flag makes it fall back:
  - any subcommand: `--override-input`, `--override-flake`, lock-writing
    flags, `--stdin`, `--repair`, `--profile`, `--redirect`
  - `eval`: `--apply`, `--write-to`
  - `build`: `--json`, `--dry-run`, `--rebuild`
  - `develop`: phase flags
- Every installable is a local flake: `.`, `./x`, `/abs`, `~/x` or `path:`,
  with no `?query` and no `^outputs`. Registry names such as `nixpkgs#hello`,
  remote refs and store paths go to nix. With `--expr` or `-f`, positional
  arguments are attribute paths and are not checked.
- `build`, `develop`, `print-dev-env` and `fmt` also need the local daemon
  socket, unless `--store` is given. iets has no store of its own.

Use `--print-out-paths` rather than `--json` for builds of local flakes, so
they stay on iets (`ekapkgs_nix::eval::build_out_paths`).

Store, profile, registry, copy and flake-lock commands always run on nix.

`system` and `home` pick their default installable by probing whether an
attribute is a derivation with `nix eval --raw <installable>.type`
(`ekapkgs_nix::eval::is_derivation`) rather than `--apply '_: null'`, so the
probe runs on iets too. Both force the value to the same depth.

`cache warm` evaluates the flake against each lock file with
`--reference-lock-file` (`ekapkgs_nix::eval::derivation_closure_paths_with_lock`).
It always passes `--no-write-lock-file` as well: nix locks the flake again
starting from the reference file, and without that flag it writes the
result over the flake's own `flake.lock`. `iets nix` does not accept
`--reference-lock-file` yet (only the native `iets` commands do), so these
evaluations run on nix.

## Selecting the evaluator

`EKAPKGS_EVALUATOR` is read once per process:

| Value            | Effect                                          |
|------------------|-------------------------------------------------|
| `auto` (default) | use iets when it is on `PATH`                   |
| `iets`           | always use iets; commands fail if it is missing |
| `nix`            | never use iets                                  |

Run with `RUST_LOG=ekapkgs_nix=debug` to see which program each command used.

## Other uses

`template` prefetches source hashes with `iets prefetch --unpack URL`, which
prints the SRI hash and adds nothing to the store. It falls back to
`nix-prefetch-url` plus `nix hash to-sri`.

ekapkgs does not use `nix-eval-jobs`. If it ever needs jobset evaluation,
`iets eval-jobs` is a drop-in replacement.

## Packaging

`nix/iets.nix` builds iets from the `iets-src` flake input (`flake = false`).
Without flakes it falls back to the revision pinned in `flake.lock`. The
client wrapper puts both `nix` and `iets` on `PATH`, and the dev shell
includes iets. To bump iets, run `nix flake update iets-src` and update
`cargoHash` in `nix/iets.nix`.
