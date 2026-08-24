use ekapkgs_nix::NixCommand;

/// Execute `ekapkgs build` by delegating to `nix build`.
///
/// In Phase 1, this is a simple pass-through. Phase 3 will add negotiated
/// substitution before the build.
pub fn execute(installable: &str, extra: &[String]) -> color_eyre::Result<()> {
    tracing::info!("Building {installable}");

    let spinner = ekapkgs_ui::progress::spinner("Evaluating...");

    let mut cmd = NixCommand::new(&["build"]).arg(installable);
    for arg in extra {
        cmd = cmd.arg(arg);
    }

    spinner.finish_and_clear();

    cmd.stream()?;
    tracing::info!("Build complete");
    Ok(())
}
