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
}
