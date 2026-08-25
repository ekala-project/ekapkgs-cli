use ekapkgs_nix::{NixCommand, NixError};

pub fn execute(installable: &str) -> color_eyre::Result<()> {
    match NixCommand::new(&["log"]).arg(installable).stream() {
        Ok(_) => Ok(()),
        Err(NixError::Failed { status, stderr }) => {
            if stderr.contains("is not available") {
                tracing::warn!("Build log not available locally");
                tracing::info!("Hint: the derivation may need to be built first");
            }
            std::process::exit(status.code().unwrap_or(1));
        },
        Err(e) => Err(e.into()),
    }
}
