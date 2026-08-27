use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "ekapkgs",
    about = "Nix CLI wrapper with negotiated binary cache"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    #[command(flatten)]
    pub verbose: clap_verbosity_flag::Verbosity,
}

#[derive(Subcommand)]
pub enum Command {
    /// Build a nix package.
    Build {
        /// The installable to build (e.g., `nixpkgs#hello`).
        installable: String,

        /// Extra arguments passed through to nix.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// Run a nix package.
    Run {
        /// The installable to run (e.g., `nixpkgs#hello`).
        installable: String,

        /// Extra arguments passed through to nix / the program.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// Enter a shell with the given packages available.
    Shell {
        /// The installable(s) to make available.
        installable: Vec<String>,

        /// Extra arguments passed through to nix.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// Enter a development environment with cache pre-fetching.
    Develop {
        /// The flake reference for the dev shell (default: `.`).
        #[arg(default_value = ".")]
        installable: String,

        /// Extra arguments passed through to nix develop.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// Manage binary caches.
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },

    /// Analyze nix closures.
    Closure {
        #[command(subcommand)]
        command: ClosureCommand,
    },

    /// Deploy a NixOS configuration to a remote host.
    Deploy {
        /// The NixOS configuration installable (e.g., `.#nixosConfigurations.prod`).
        installable: String,

        /// Target host to deploy to (SSH destination).
        #[arg(long)]
        target_host: String,

        /// Host to build on (default: local machine).
        #[arg(long)]
        build_host: Option<String>,

        /// Activation mode.
        #[arg(long, value_enum, default_value = "switch")]
        mode: ActivationMode,

        /// Only show what would be done.
        #[arg(long)]
        dry_run: bool,
    },

    /// Flake introspection and management.
    Flake {
        #[command(subcommand)]
        command: FlakeCommand,
    },

    /// Manage the local nix store.
    Store {
        #[command(subcommand)]
        command: StoreCommand,
    },

    /// Show build log for a derivation.
    Log {
        /// The installable to show logs for.
        installable: String,
    },

    /// Show what will be built vs substituted (dry run).
    DryRun {
        /// The installable to analyze.
        installable: String,

        /// Extra arguments passed through to nix.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// Manage per-user home configurations.
    Home {
        #[command(subcommand)]
        command: HomeCommand,
    },

    /// Manage the local system configuration (nixos-rebuild replacement).
    System {
        #[command(subcommand)]
        command: SystemCommand,
    },

    /// Check system health and configuration.
    Doctor,

    /// Run a local substituter proxy for transparent nix integration.
    ///
    /// Starts an HTTP server implementing the nix binary cache protocol that
    /// batches narinfo queries and negotiates with the upstream ekapkgs server.
    /// Configure nix to use it: `substituters = http://localhost:PORT`
    Substituter {
        /// Port to bind the local proxy on.
        #[arg(short, long, default_value = "7422")]
        port: u16,

        /// Upstream ekapkgs server URL.
        #[arg(long)]
        upstream: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum CacheCommand {
    /// Push store paths or packages to a binary cache.
    Push {
        /// Store paths or installables to push.
        paths: Vec<String>,

        /// Cache URL to push to (overrides config).
        #[arg(long)]
        cache: Option<String>,

        /// Transfer only sources and derivation graph (for low-bandwidth links).
        /// The remote machine rebuilds from source instead of receiving built NARs.
        #[arg(long)]
        sources_only: bool,
    },

    /// Pull (pre-fetch) store paths from a binary cache.
    Pull {
        /// Store paths or installables to pull.
        paths: Vec<String>,

        /// Cache URL to pull from (overrides config).
        #[arg(long)]
        cache: Option<String>,
    },

    /// Pre-warm the cache by downloading the closure diff between two flake.lock versions.
    Warm {
        /// The installable to evaluate (e.g., `.#nixosConfigurations.prod`).
        installable: String,

        /// Git revision range for flake.lock diff (e.g., `HEAD~1..HEAD`).
        #[arg(long)]
        from_flake_lock_diff: Option<String>,

        /// Path to the old flake.lock file (alternative to --from-flake-lock-diff).
        #[arg(long)]
        old: Option<String>,

        /// Path to the new flake.lock file (default: ./flake.lock).
        #[arg(long)]
        new: Option<String>,

        /// Cache URL (overrides config).
        #[arg(long)]
        cache: Option<String>,
    },

    /// Configure cache authentication.
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
}

#[derive(Subcommand)]
pub enum AuthCommand {
    /// Set a push token for a cache.
    Login {
        /// Cache URL to authenticate with.
        cache: String,

        /// Bearer token for push access.
        #[arg(long)]
        token: String,
    },

    /// Remove stored credentials for a cache.
    Logout {
        /// Cache URL to remove credentials for.
        cache: String,
    },

    /// Show configured caches and auth status.
    Status,
}

#[derive(Subcommand)]
pub enum ClosureCommand {
    /// Show closure size breakdown.
    Size {
        /// The installable to analyze (e.g., `nixpkgs#hello`).
        installable: String,
    },

    /// Trace why a package depends on another.
    WhyDepends {
        /// The package to analyze.
        installable: String,

        /// The dependency to trace.
        dependency: String,
    },

    /// Diff two closures.
    Diff {
        /// First installable or store path.
        a: String,

        /// Second installable or store path.
        b: String,
    },
}

#[derive(Subcommand)]
pub enum StoreCommand {
    /// Garbage collect the nix store.
    Gc {
        /// Delete paths older than this duration (e.g., `30d`, `7d`).
        #[arg(long)]
        older_than: Option<String>,

        /// Only show what would be deleted.
        #[arg(long)]
        dry_run: bool,
    },

    /// Optimize the store by deduplicating via hardlinks.
    Optimize,

    /// Verify store integrity.
    Verify {
        /// Check all paths in the store.
        #[arg(long)]
        all: bool,

        /// Attempt to repair invalid paths.
        #[arg(long)]
        repair: bool,

        /// Minimum number of valid signatures required.
        #[arg(long)]
        sigs_needed: Option<u32>,
    },
}

#[derive(Subcommand)]
pub enum FlakeCommand {
    /// Pretty-print flake outputs tree.
    Show {
        /// Flake reference (default: current directory).
        #[arg(default_value = ".")]
        flake_ref: String,
    },

    /// Show flake input dependency tree.
    Metadata {
        /// Flake reference (default: current directory).
        #[arg(default_value = ".")]
        flake_ref: String,
    },

    /// Update a flake input and show closure size diff.
    UpdateDiff {
        /// The flake input to update.
        input: String,

        /// Installable to evaluate for closure comparison (default: `.`).
        #[arg(long, default_value = ".")]
        installable: String,
    },
}

#[derive(Subcommand)]
pub enum HomeCommand {
    /// Build and activate the home configuration.
    Switch {
        /// The installable for the home configuration
        /// (e.g., `.#config.system.build.home`).
        #[arg(default_value = ".#config.system.build.home")]
        installable: String,

        /// Extra arguments passed through to nix build.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// Build the home configuration without activating.
    Build {
        /// The installable for the home configuration
        /// (e.g., `.#config.system.build.home`).
        #[arg(default_value = ".#config.system.build.home")]
        installable: String,

        /// Extra arguments passed through to nix build.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// List home configuration generations.
    Generations,

    /// List packages installed via home configuration.
    Packages,
}

#[derive(Subcommand)]
pub enum SystemCommand {
    /// Build the system configuration and activate it.
    ///
    /// Builds the system toplevel, updates the system profile, installs
    /// boot entries, and activates the new configuration.
    Switch {
        /// The system configuration installable
        /// (e.g., `.#nixosConfigurations.myhost.config.system.build.toplevel`).
        #[arg(default_value = ".#config.system.build.toplevel")]
        installable: String,

        /// Only show what would be done.
        #[arg(long)]
        dry_run: bool,

        /// Extra arguments passed through to nix build.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// Build and activate, but only add the boot entry without switching.
    ///
    /// The new configuration becomes the default boot entry but the
    /// running system is not changed until the next reboot.
    Boot {
        /// The system configuration installable.
        #[arg(default_value = ".#config.system.build.toplevel")]
        installable: String,

        /// Extra arguments passed through to nix build.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// Build and activate without updating the boot entry.
    ///
    /// Useful for testing a configuration without committing to it
    /// across reboots.
    Test {
        /// The system configuration installable.
        #[arg(default_value = ".#config.system.build.toplevel")]
        installable: String,

        /// Extra arguments passed through to nix build.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// Build the system configuration without activating.
    Build {
        /// The system configuration installable.
        #[arg(default_value = ".#config.system.build.toplevel")]
        installable: String,

        /// Extra arguments passed through to nix build.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

    /// List system generations.
    ListGenerations,

    /// Roll back to the previous system generation.
    Rollback {
        /// Only show what would be done.
        #[arg(long)]
        dry_run: bool,
    },

    /// Remove boot entries for generations that no longer exist.
    ///
    /// After garbage-collecting old system generations with
    /// `ekapkgs store gc`, their boot entries remain on the ESP.
    /// This command removes those orphaned entries and their
    /// associated kernel/initrd/UKI files.
    ///
    /// Use `--gc` to run garbage collection first, removing old
    /// profile generations before pruning their boot entries.
    PruneBootEntries {
        /// Boot mount point (ESP or XBOOTLDR).
        #[arg(long, default_value = "/boot")]
        boot_mount: String,

        /// Run `nix-collect-garbage -d` before pruning to remove
        /// old generation profile links first.
        #[arg(long)]
        gc: bool,

        /// Only show what would be removed.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Clone, clap::ValueEnum)]
pub enum ActivationMode {
    Switch,
    Boot,
    Test,
}
