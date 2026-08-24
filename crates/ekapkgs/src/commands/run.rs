use ekapkgs_nix::NixCommand;

/// Execute `ekapkgs run` by delegating to `nix run`.
///
/// In Phase 1, this is a simple pass-through that execs into nix.
/// Phase 3 will add negotiated substitution before execution.
pub fn execute(installable: &str, extra: &[String]) -> color_eyre::Result<()> {
    tracing::info!("Running {installable}");

    let mut cmd = NixCommand::new(&["run"]).arg(installable);

    if !extra.is_empty() {
        cmd = cmd.arg("--");
        for arg in extra {
            cmd = cmd.arg(arg);
        }
    }

    // exec replaces the process — this only returns on error
    let err = cmd.exec().unwrap_err();
    Err(err.into())
}
