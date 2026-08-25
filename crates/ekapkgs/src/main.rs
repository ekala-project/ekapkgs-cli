mod cli;
mod commands;
mod config;
mod download;
mod negotiate;
mod prefetch;

use clap::Parser;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let args = cli::Cli::parse();
    ekapkgs_ui::logging::init(&args.verbose);
    commands::run(args.command)
}
