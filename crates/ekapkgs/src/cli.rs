use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ekapkgs", about = "Nix CLI wrapper with negotiated binary cache")]
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
    },

    /// Pull (pre-fetch) store paths from a binary cache.
    Pull {
        /// Store paths or installables to pull.
        paths: Vec<String>,

        /// Cache URL to pull from (overrides config).
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
