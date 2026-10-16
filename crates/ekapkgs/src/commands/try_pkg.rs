use ekapkgs_nix::NixCommand;

/// Execute `ekapkgs try` by delegating to `nix shell`.
///
/// Resolves package names against the given flake and either runs the
/// specified command or drops into an interactive shell.
pub fn execute(packages: &[String], flake: &str, command: &[String]) -> color_eyre::Result<()> {
    let installables: Vec<String> = packages
        .iter()
        .map(|pkg| {
            if pkg.contains('#') || pkg.contains('/') {
                // Already a full installable reference.
                pkg.clone()
            } else {
                format!("{flake}#{pkg}")
            }
        })
        .collect();

    let pkgs_display = packages.join(", ");
    if command.is_empty() {
        tracing::info!("Trying {pkgs_display} (interactive shell)...");
    } else {
        tracing::info!("Trying {pkgs_display}...");
    }

    let mut cmd = NixCommand::new(&["shell"]);
    for inst in &installables {
        cmd = cmd.arg(inst);
    }

    if !command.is_empty() {
        cmd = cmd.arg("--command");
        for arg in command {
            cmd = cmd.arg(arg);
        }
    }

    let err = cmd.exec().unwrap_err();
    Err(err.into())
}
