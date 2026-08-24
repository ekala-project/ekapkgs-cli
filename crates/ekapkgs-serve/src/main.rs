use clap::Parser;

#[derive(Parser)]
#[command(name = "ekapkgs-serve", about = "ekapkgs binary cache server")]
struct Cli {
    #[command(flatten)]
    verbose: clap_verbosity_flag::Verbosity,
}

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    ekapkgs_ui::logging::init(&cli.verbose);
    tracing::info!("ekapkgs-serve is not yet implemented — see Phase 2");
    Ok(())
}
