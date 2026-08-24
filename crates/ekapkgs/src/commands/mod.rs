mod build;
mod cache;
mod run;
mod shell;

use crate::cli::Command;

pub fn run(command: Command) -> color_eyre::Result<()> {
    match command {
        Command::Build { installable, extra } => build::execute(&installable, &extra),
        Command::Run { installable, extra } => run::execute(&installable, &extra),
        Command::Shell { installable, extra } => shell::execute(&installable, &extra),
        Command::Cache { command } => cache::execute(command),
    }
}
