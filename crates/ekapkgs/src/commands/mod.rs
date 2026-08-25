mod build;
mod cache;
mod doctor;
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
        Command::Doctor => doctor::execute(),
        Command::Substituter { port, upstream } => substituter::execute(port, upstream),
    }
}
