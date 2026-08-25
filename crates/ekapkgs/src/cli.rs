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
