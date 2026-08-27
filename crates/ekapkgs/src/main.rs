mod cli;
mod commands;
mod config;
mod download;
mod negotiate;
mod prefetch;

use clap::{CommandFactory, Parser};

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
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
