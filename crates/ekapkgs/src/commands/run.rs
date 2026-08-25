use ekapkgs_nix::installable::Installable;
use ekapkgs_nix::{NixCommand, eval, store};

use crate::config::ClientConfig;

/// Execute `ekapkgs run` — negotiated substitution then exec into the result.
pub fn execute(installable: &str, extra: &[String]) -> color_eyre::Result<()> {
    let config = ClientConfig::load()?;
    let inst = Installable::new(installable);

    // Try negotiated substitution if a cache is configured.
    if let Some(cache) = config.primary_cache() {
        tracing::info!("Running {installable}");

        let spinner = ekapkgs_ui::progress::spinner("Evaluating closure...");
        let closure_paths = eval::derivation_closure_paths(&inst)?;

        // Identify the primary target output path for critical path prioritization.
        let target_hash = eval::eval_build_outputs(&inst)
            .ok()
            .and_then(|outputs| {
                outputs
                    .first()
                    .and_then(|o| o.outputs.values().next().cloned())
            })
            .and_then(|path| store::store_path_hash(&path).map(String::from));

        spinner.set_message("Checking local store...");
        let (have, want) = store::partition_local(&closure_paths)?;
        spinner.finish_and_clear();

        if !want.is_empty() {
            tracing::info!(
                "Closure: {} paths ({} in local store, {} to fetch)",
                closure_paths.len(),
                have.len(),
                want.len()
            );

            let want_hashes: Vec<String> = want
                .iter()
                .filter_map(|p| store::store_path_hash(p).map(String::from))
                .collect();
            let have_hashes: Vec<String> = have
                .iter()
                .filter_map(|p| store::store_path_hash(p).map(String::from))
                .collect();

            let server_url = cache.url.clone();

            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                let spinner = ekapkgs_ui::progress::spinner("Negotiating with cache...");

                let response = crate::negotiate::negotiate_with_target(
                    &server_url,
                    want_hashes,
                    have_hashes,
                    target_hash.as_deref(),
                )
                .await?;

                spinner.finish_and_clear();

                if !response.available.is_empty() {
                    tracing::info!("{} paths to download", response.available.len());
                    crate::download::download_and_import(
                        &server_url,
                        &response,
                        config.defaults.max_parallel_downloads,
                    )
                    .await?;
                }

                Ok::<(), color_eyre::Report>(())
            })?;
        }
    }

    // Exec into nix run (replaces this process).
    let mut cmd = NixCommand::new(&["run"]).arg(installable);
    if !extra.is_empty() {
        cmd = cmd.arg("--");
        for arg in extra {
            cmd = cmd.arg(arg);
        }
    }

    let err = cmd.exec().unwrap_err();
    Err(err.into())
}
