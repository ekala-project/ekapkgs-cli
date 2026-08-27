use ekapkgs_nix::NixCommand;

use crate::cli::RegistryCommand;

pub fn execute(command: RegistryCommand) -> color_eyre::Result<()> {
    match command {
        RegistryCommand::List => cmd_list(),
        RegistryCommand::Add { from, to, registry } => cmd_add(&from, &to, registry.as_deref()),
        RegistryCommand::Remove { entry, registry }
        | RegistryCommand::Unpin { entry, registry } => cmd_remove(&entry, registry.as_deref()),
        RegistryCommand::Pin { entry, registry } => cmd_pin(&entry, registry.as_deref()),
        RegistryCommand::Resolve { refs } => cmd_resolve(&refs),
    }
}

fn cmd_list() -> color_eyre::Result<()> {
    NixCommand::new(&["registry", "list"]).stream()?;
    Ok(())
}

fn cmd_add(from: &str, to: &str, registry: Option<&str>) -> color_eyre::Result<()> {
    let mut cmd = NixCommand::new(&["registry", "add"]);

    if let Some(reg) = registry {
        cmd = cmd.arg("--registry").arg(reg);
    }

    cmd.arg(from).arg(to).stream()?;
    Ok(())
}

fn cmd_remove(entry: &str, registry: Option<&str>) -> color_eyre::Result<()> {
    let mut cmd = NixCommand::new(&["registry", "remove"]);

    if let Some(reg) = registry {
        cmd = cmd.arg("--registry").arg(reg);
    }

    cmd.arg(entry).stream()?;
    Ok(())
}

fn cmd_pin(entry: &str, registry: Option<&str>) -> color_eyre::Result<()> {
    let mut cmd = NixCommand::new(&["registry", "pin"]);

    if let Some(reg) = registry {
        cmd = cmd.arg("--registry").arg(reg);
    }

    cmd.arg(entry).stream()?;
    Ok(())
}

fn cmd_resolve(refs: &[String]) -> color_eyre::Result<()> {
    NixCommand::new(&["registry", "resolve"])
        .args(refs.iter().map(String::as_str))
        .stream()?;
    Ok(())
}
