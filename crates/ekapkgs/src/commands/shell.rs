use ekapkgs_nix::NixCommand;

/// Execute `ekapkgs shell` by delegating to `nix shell`.
pub fn execute(installables: &[String], extra: &[String]) -> color_eyre::Result<()> {
    let pkgs = installables.join(", ");
    tracing::info!("Entering shell with {pkgs}");

    let mut cmd = NixCommand::new(&["shell"]);
    for inst in installables {
        cmd = cmd.arg(inst);
    }
    for arg in extra {
        cmd = cmd.arg(arg);
    }

    let err = cmd.exec().unwrap_err();
    Err(err.into())
}
