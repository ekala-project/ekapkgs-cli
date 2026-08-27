mod build;
mod cache;
mod closure;
mod deploy;
mod develop;
mod doctor;
mod dry_run;
mod flake;
mod home;
mod log;
mod run;
mod search;
mod shell;
mod store;
mod substituter;
mod system;

use crate::cli::Command;

pub fn run(command: Command) -> color_eyre::Result<()> {
    match command {
        Command::Build { installable, extra } => build::execute(&installable, &extra),
        Command::Run { installable, extra } => run::execute(&installable, &extra),
        Command::Shell { installable, extra } => shell::execute(&installable, &extra),
        Command::Develop { installable, extra } => develop::execute(&installable, &extra),
        Command::Deploy {
            installable,
            target_host,
            build_host,
            mode,
            dry_run,
        } => deploy::execute(
            &installable,
            &target_host,
            build_host.as_deref(),
            &mode,
            dry_run,
        ),
        Command::Home { command } => home::execute(command),
        Command::System { command } => system::execute(command),
        Command::Search { command } => search::execute(command),
        Command::Cache { command } => cache::execute(command),
        Command::Closure { command } => closure::execute(command),
        Command::Flake { command } => flake::execute(command),
        Command::Store { command } => store::execute(command),
        Command::Log { installable } => log::execute(&installable),
        Command::DryRun { installable, extra } => dry_run::execute(&installable, &extra),
        Command::Doctor => doctor::execute(),
        Command::Substituter { port, upstream } => substituter::execute(port, upstream),
    }
}
