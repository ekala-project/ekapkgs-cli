use ekapkgs_nix::NixCommand;
use ekapkgs_nix::installable::Installable;

use crate::config::ClientConfig;

/// Execute `ekapkgs develop` — cache pre-fetch then exec into nix develop.
pub fn execute(installable: &str, extra: &[String]) -> color_eyre::Result<()> {
    let config = ClientConfig::load()?;
    let inst = Installable::new(installable);

    if let Some(cache) = config.primary_cache() {
        tracing::info!("Preparing dev environment for {installable}");

        let spinner = ekapkgs_ui::progress::spinner("Evaluating dev shell closure...");
        let closure_paths = crate::build_closure::build_closure_paths(
            &cache.url,
            &inst,
            crate::build_closure::Goal::Inputs,
        )?;
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
