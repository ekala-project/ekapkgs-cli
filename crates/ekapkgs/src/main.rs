mod background_apply;
mod cas_pull;
mod chunk_store;
mod cli;
mod commands;
mod completions;
mod config;
mod download;
mod negotiate;
mod package_validate;
mod prefetch;
pub mod service_schema;
mod store_path_index;
mod symlink_dir;

use clap::{CommandFactory, Parser};

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;

    // Dynamic completions: when the shell invokes us with COMPLETE=<shell>,
    // serve completions and exit. This must run before Cli::parse().
    clap_complete::CompleteEnv::with_factory(cli::Cli::command).complete();

    let args = cli::Cli::parse();

    if let cli::Command::Completions { shell } = &args.command {
        clap_complete::generate(
            *shell,
            &mut cli::Cli::command(),
            "ekapkgs",
            &mut std::io::stdout(),
        );
        return Ok(());
    }

    ekapkgs_ui::logging::init(&args.verbose);
    commands::run(args.command)
}
