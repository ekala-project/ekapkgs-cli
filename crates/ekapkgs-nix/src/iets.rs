//! Run the evaluating `nix` subcommands through iets.
//!
//! [iets](https://github.com/numtide/iets) re-implements the Nix evaluator
//! and the `nix` subcommands that evaluate. Run as `iets nix SUBCOMMAND ...`
//! (its "classic mode"), it behaves like `nix SUBCOMMAND ...`, with the same
//! stdout, JSON and exit codes. For anything it does not implement, it prints
//! `iets: falling back to nix: REASON` on stderr and execs the real `nix`.
//!
//! [`NixCommand`](crate::NixCommand) asks [`route`] which program to run.
//! A command goes to iets only when iets is installed and handles the command
//! itself. Everything else goes straight to `nix`, so it skips the extra exec
//! and the fallback notice. These checks only save that work. iets still
//! falls back on its own for any case they miss.
//!
//! `EKAPKGS_EVALUATOR` picks the mode: `auto` (the default) uses iets when it
//! is on `PATH`, `iets` always uses it, and `nix` never does.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Environment variable that selects the evaluator.
pub const EVALUATOR_ENV: &str = "EKAPKGS_EVALUATOR";

const STORE_DIR: &str = "/nix/store/";

/// The program a [`NixCommand`](crate::NixCommand) runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Program {
    /// `nix ARGS...`
    Nix,
    /// `iets nix ARGS...`, with the iets binary at this path.
    Iets(&'static Path),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Auto,
    Iets,
    Nix,
}

impl Mode {
    fn from_env(value: Option<OsString>) -> Self {
        let Some(value) = value else {
            return Self::Auto;
        };
        match value.to_str() {
            Some("" | "auto") => Self::Auto,
            Some("iets") => Self::Iets,
            Some("nix") => Self::Nix,
            _ => {
                tracing::warn!(
                    "{EVALUATOR_ENV}={} is not one of auto, iets or nix; using auto",
                    value.to_string_lossy()
                );
                Self::Auto
            },
        }
    }
}

/// The iets binary ekapkgs uses, or `None` when every command goes to `nix`.
///
/// Resolved once per process.
pub fn binary() -> Option<&'static Path> {
    static IETS: OnceLock<Option<PathBuf>> = OnceLock::new();
    IETS.get_or_init(|| {
        let found = which::which("iets").ok();
        let iets = match Mode::from_env(std::env::var_os(EVALUATOR_ENV)) {
            Mode::Nix => None,
            Mode::Iets => Some(found.unwrap_or_else(|| {
                // Run it by name anyway, so the commands fail rather than
                // quietly use nix.
                tracing::warn!("{EVALUATOR_ENV}=iets, but iets is not on PATH");
                PathBuf::from("iets")
            })),
            Mode::Auto => found,
        };
        tracing::debug!(iets = ?iets, "evaluator");
        iets
    })
    .as_deref()
}

/// Pick the program that runs `nix ARGV...`.
pub fn route(argv: &[&str]) -> Program {
    match binary() {
        Some(iets) if handles(argv, daemon_available) => Program::Iets(iets),
        _ => Program::Nix,
    }
}

/// Whether `iets nix ARGV...` evaluates natively rather than falling back.
///
/// Covers the classic-mode rules in iets's `docs/reference/classic.md` for
/// the arguments ekapkgs passes. `daemon` reports whether the local Nix
/// daemon is up, since iets has no store of its own.
fn handles(argv: &[&str], daemon: impl FnOnce() -> bool) -> bool {
    let Some(cmd) = Classic::parse(argv) else {
        return false;
    };
    if !cmd.native() {
        return false;
    }
    // `--store` names where the store is, so the daemon may not matter.
    !cmd.needs_store() || cmd.has_flag("--store") || daemon()
}

/// The subcommands iets implements. Every other one runs the real `nix`.
const NATIVE: &[&[&str]] = &[
    &["eval"],
    &["build"],
    &["derivation", "show"],
    &["search"],
    &["print-dev-env"],
    &["develop"],
    &["fmt"],
    &["flake", "prefetch-inputs"],
];

/// Flags that make every native subcommand fall back.
const FALLBACK_FLAGS: &[&str] = &[
    "--override-input",
    "--override-flake",
    "--update-input",
    "--recreate-lock-file",
    "--no-update-lock-file",
    "--no-write-lock-file",
    "--commit-lock-file",
    "--stdin",
    "--repair",
    "--profile",
    "--redirect",
];

/// Flags that make one subcommand fall back.
fn falls_back_on(subcommand: &[&str], flag: &str) -> bool {
    match subcommand {
        ["eval"] => matches!(flag, "--apply" | "--write-to"),
        ["build"] => matches!(flag, "--json" | "--dry-run" | "--rebuild"),
        ["develop"] => matches!(
            flag,
            "--phase"
                | "--unpack"
                | "--configure"
                | "--build"
                | "--check"
                | "--install"
                | "--installcheck"
        ),
        _ => false,
    }
}

/// How many values follow a flag.
///
/// Only the flags ekapkgs and iets care about. An unknown flag counts as
/// taking none, which at worst checks a flag value as if it were an
/// installable.
fn arity(subcommand: &[&str], flag: &str) -> usize {
    match flag {
        "--arg" | "--argstr" | "--option" | "--override-input" | "--override-flake"
        | "--redirect" => 2,
        "--apply"
        | "--expr"
        | "-f"
        | "--file"
        | "-I"
        | "--include"
        | "-o"
        | "--out-link"
        | "--log-format"
        | "--store"
        | "--eval-store"
        | "-j"
        | "--max-jobs"
        | "--cores"
        | "--profile"
        | "--reference-lock-file"
        | "--update-input"
        | "--write-to"
        | "--flake-registry"
        | "--experimental-features"
        | "--extra-experimental-features"
        | "-e"
        | "--exclude"
        | "--phase"
        | "--keep"
        | "--keep-env-var"
        | "-u"
        | "--unset"
        | "--unset-env-var" => 1,
        // Under `nix develop`, `-k` keeps a variable; elsewhere it is `--keep-going`.
        "-k" if subcommand == ["develop"] => 1,
        _ => 0,
    }
}

/// A `nix` command line, split the way iets's classic mode reads it.
#[derive(Debug)]
struct Classic<'a> {
    subcommand: Vec<&'a str>,
    flags: Vec<&'a str>,
    /// Positional arguments: installables, or attribute paths with
    /// `--expr`/`--file`. `nix search` adds its regexes here too.
    positional: Vec<&'a str>,
}

impl<'a> Classic<'a> {
    fn parse(argv: &[&'a str]) -> Option<Self> {
        let mut args = argv.iter().copied();

        // Global flags may come before the subcommand.
        let mut flags = Vec::new();
        let first = loop {
            let arg = args.next()?;
            if !arg.starts_with('-') {
                break arg;
            }
            flags.push(arg);
            for _ in 0..arity(&[], arg) {
                args.next();
            }
        };
        let mut subcommand = vec![first];
        let mut args = args.peekable();
        if matches!(first, "derivation" | "flake") {
            subcommand.push(args.next()?);
        }

        let mut positional = Vec::new();
        while let Some(arg) = args.next() {
            if arg == "--" {
                break;
            }
            // `nix develop -c CMD ARGS...`: the rest is the command.
            if subcommand == ["develop"] && matches!(arg, "-c" | "--command") {
                flags.push(arg);
                break;
            }
            if arg.starts_with('-') && arg.len() > 1 {
                flags.push(arg);
                for _ in 0..arity(&subcommand, arg) {
                    args.next();
                }
            } else {
                positional.push(arg);
            }
        }

        Some(Self {
            subcommand,
            flags,
            positional,
        })
    }

    fn has_flag(&self, flag: &str) -> bool {
        self.flags.contains(&flag)
    }

    fn native(&self) -> bool {
        if !NATIVE.contains(&self.subcommand.as_slice()) {
            return false;
        }
        if self
            .flags
            .iter()
            .any(|f| FALLBACK_FLAGS.contains(f) || falls_back_on(&self.subcommand, f))
        {
            return false;
        }
        self.installables().all(is_local)
    }

    /// The positional arguments that are flake references.
    fn installables(&self) -> impl Iterator<Item = &'a str> + '_ {
        // With `--expr` or `--file`, positional arguments are attribute paths.
        let from_flakes = !["--expr", "-f", "--file"].iter().any(|f| self.has_flag(f));
        let take = match self.subcommand.as_slice() {
            _ if !from_flakes => 0,
            // `nix search INSTALLABLE REGEX...`
            ["search"] => 1,
            // `nix fmt` always formats `.`; its arguments go to the formatter.
            ["fmt"] => 0,
            _ => usize::MAX,
        };
        self.positional.iter().copied().take(take)
    }

    /// Whether the subcommand talks to the store, which for iets is always a
    /// daemon or a remote builder.
    fn needs_store(&self) -> bool {
        matches!(
            self.subcommand.as_slice(),
            ["build"] | ["develop"] | ["print-dev-env"] | ["fmt"]
        )
    }
}

/// Whether iets evaluates this flake reference itself.
///
/// iets reads only local flakes: `.`, `./x`, `/abs`, `~/x` and `path:`.
/// It hands registry names (`nixpkgs#hello`), remote references
/// (`github:`, `git+https:`, ...), references with a `?query`, store paths,
/// and `^output` selections to the real nix.
fn is_local(installable: &str) -> bool {
    let flake_ref = installable
        .split_once('#')
        .map_or(installable, |(flake, _)| flake);
    if installable.contains('^') || flake_ref.contains('?') || flake_ref.starts_with(STORE_DIR) {
        return false;
    }
    flake_ref.starts_with(['.', '/', '~']) || flake_ref.starts_with("path:")
}

/// Whether the local Nix daemon's socket exists, found the way iets finds it.
fn daemon_available() -> bool {
    let socket = std::env::var_os("NIX_DAEMON_SOCKET_PATH").map_or_else(
        || {
            std::env::var_os("NIX_STATE_DIR")
                .map_or_else(|| PathBuf::from("/nix/var/nix"), PathBuf::from)
                .join("daemon-socket/socket")
        },
        PathBuf::from,
    );
    socket.exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native(argv: &[&str]) -> bool {
        handles(argv, || true)
    }

    #[test]
    fn local_flakes_are_native() {
        assert!(native(&["eval", ".#foo", "--json"]));
        assert!(native(&["build", "./sub#pkg", "--print-out-paths"]));
        assert!(native(&["build", "/etc/nixos#toplevel", "--no-link"]));
        assert!(native(&["build", "path:/tmp/x#y"]));
        assert!(native(&["build", "~/src/flake#y"]));
        assert!(native(&["derivation", "show", "-r", ".#foo"]));
        assert!(native(&["print-dev-env", "--json", ".#default"]));
        assert!(native(&["develop", ".#default"]));
        assert!(native(&["search", ".", "--json", "^"]));
        assert!(native(&["flake", "prefetch-inputs", "."]));
        assert!(native(&["fmt", "--", "--check"]));
    }

    #[test]
    fn default_installable_is_native() {
        // `nix build` with no installable builds `.`.
        assert!(native(&["build"]));
        assert!(native(&["develop"]));
    }

    #[test]
    fn remote_and_registry_refs_go_to_nix() {
        assert!(!native(&["build", "nixpkgs#hello"]));
        assert!(!native(&["build", "nixpkgs/nixos-unstable#hello"]));
        assert!(!native(&["eval", "github:NixOS/nixpkgs#hello.name"]));
        assert!(!native(&["build", "git+file:///src/repo#pkg"]));
        assert!(!native(&["build", "flake:nixpkgs#hello"]));
        assert!(!native(&["search", "nixpkgs", "--json", "^"]));
        assert!(!native(&["develop", "nixpkgs#hello"]));
        assert!(!native(&["build", ".?dir=sub#pkg"]));
    }

    #[test]
    fn store_paths_and_output_specs_go_to_nix() {
        assert!(!native(&["derivation", "show", "/nix/store/abc-foo.drv"]));
        assert!(!native(&["build", "/nix/store/abc-foo.drv^out"]));
        assert!(!native(&["build", ".#foo^out"]));
    }

    #[test]
    fn unsupported_subcommands_go_to_nix() {
        for args in [
            &["path-info", "--json", "/nix/store/abc-foo"][..],
            &["flake", "update"],
            &["flake", "metadata", "--json", "."],
            &["flake", "show", ".", "--json"],
            &["profile", "install", ".#foo"],
            &["copy", "--to", "ssh-ng://host", "/nix/store/abc"],
            &["store", "diff-closures", "/a", "/b"],
            &["run", ".#foo"],
            &["shell", ".#foo"],
            &["log", ".#foo"],
            &["why-depends", ".#a", ".#b"],
            &["registry", "list"],
            &["--version"],
        ] {
            assert!(!native(args), "{args:?}");
        }
    }

    #[test]
    fn fallback_flags_go_to_nix() {
        assert!(!native(&["eval", ".#foo", "--apply", "_: null"]));
        assert!(!native(&["build", ".#foo", "--json"]));
        assert!(!native(&["build", ".#foo", "--dry-run"]));
        assert!(!native(&[
            "print-dev-env",
            "--json",
            "--override-input",
            "nixpkgs",
            "./nixpkgs",
            ".#default",
        ]));
        assert!(!native(&["develop", ".#default", "--phase", "build"]));
    }

    #[test]
    fn json_is_fine_outside_build() {
        assert!(native(&["eval", "--json", ".#foo"]));
        assert!(native(&["search", ".", "--json", "^"]));
    }

    #[test]
    fn expressions_take_attribute_paths() {
        assert!(native(&["eval", "--raw", "--impure", "--expr", "1 + 1"]));
        assert!(native(&["build", "-f", "default.nix", "hello"]));
        // The expression text is a flag value, not an installable.
        assert!(native(&[
            "eval",
            "--impure",
            "--expr",
            "builtins.getFlake \"github:a/b\"",
        ]));
    }

    #[test]
    fn flag_values_are_not_installables() {
        assert!(native(&[
            "build",
            ".#foo",
            "--option",
            "substituters",
            "https://cache.example",
        ]));
        assert!(native(&["build", "--log-format", "internal-json", ".#foo"]));
        assert!(native(&["build", ".#foo", "-o", "result-foo"]));
    }

    #[test]
    fn develop_command_is_not_installable() {
        assert!(native(&["develop", ".#default", "-c", "nixpkgs#hello"]));
        assert!(native(&["develop", ".#default", "--command", "make", "-j"]));
    }

    #[test]
    fn store_commands_need_the_daemon() {
        let no_daemon = || false;
        assert!(!handles(&["build", ".#foo"], no_daemon));
        assert!(!handles(&["develop", ".#foo"], no_daemon));
        assert!(!handles(&["print-dev-env", ".#foo"], no_daemon));
        assert!(handles(
            &["build", ".#foo", "--store", "ssh-ng://b"],
            no_daemon
        ));
        // Evaluation alone needs no store.
        assert!(handles(&["eval", ".#foo"], no_daemon));
        assert!(handles(&["derivation", "show", ".#foo"], no_daemon));
    }

    #[test]
    fn mode_from_env() {
        assert_eq!(Mode::from_env(None), Mode::Auto);
        assert_eq!(Mode::from_env(Some("".into())), Mode::Auto);
        assert_eq!(Mode::from_env(Some("auto".into())), Mode::Auto);
        assert_eq!(Mode::from_env(Some("iets".into())), Mode::Iets);
        assert_eq!(Mode::from_env(Some("nix".into())), Mode::Nix);
        assert_eq!(Mode::from_env(Some("lix".into())), Mode::Auto);
    }
}
