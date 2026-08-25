mod build;
mod cache;
mod closure;
mod doctor;
mod dry_run;
mod log;
mod run;
mod shell;
mod substituter;

use crate::cli::Command;

pub fn run(command: Command) -> color_eyre::Result<()> {
    match command {
        Command::Build { installable, extra } => build::execute(&installable, &extra),
        Command::Run { installable, extra } => run::execute(&installable, &extra),
        Command::Shell { installable, extra } => shell::execute(&installable, &extra),
        Command::Cache { command } => cache::execute(command),
        Command::Closure { command } => closure::execute(command),
        Command::Log { installable } => log::execute(&installable),
        Command::DryRun { installable, extra } => dry_run::execute(&installable, &extra),
        Command::Doctor => doctor::execute(),
        Command::Substituter { port, upstream } => substituter::execute(port, upstream),
    }
}
