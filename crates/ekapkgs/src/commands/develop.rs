use ekapkgs_nix::installable::Installable;
use ekapkgs_nix::{NixCommand, eval};

use crate::config::ClientConfig;

/// Execute `ekapkgs develop` — cache pre-fetch then exec into nix develop.
pub fn execute(installable: &str, extra: &[String]) -> color_eyre::Result<()> {
    let config = ClientConfig::load()?;
    let inst = Installable::new(installable);

    if config.primary_cache().is_some() {
        tracing::info!("Preparing dev environment for {installable}");

        let spinner = ekapkgs_ui::progress::spinner("Evaluating dev shell closure...");
        let closure_paths = eval::derivation_closure_paths(&inst)?;
        spinner.finish_and_clear();

        crate::prefetch::prefetch_closure(&config, &closure_paths)?;
    }

    // Exec into nix develop (replaces this process).
    let mut cmd = NixCommand::new(&["develop"]).arg(installable);
    for arg in extra {
        cmd = cmd.arg(arg);
    }

    let err = cmd.exec().unwrap_err();
    Err(err.into())
}
