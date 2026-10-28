use ekapkgs_nix::installable::Installable;
use ekapkgs_nix::{NixCommand, eval, store};

use crate::config::ClientConfig;

/// Execute `ekapkgs run` — negotiated substitution then exec into the result.
pub fn execute(installable: &str, extra: &[String]) -> color_eyre::Result<()> {
    let config = ClientConfig::load()?;
    let inst = Installable::new(installable);

    // Try negotiated substitution if a cache is configured.
    if let Some(cache) = config.primary_cache() {
        // Verify the cache server is reachable before doing expensive work.
        let rt = tokio::runtime::Runtime::new()?;
        let server_url = cache.url.clone();

        let server_ok = rt.block_on(async {
            let spinner = ekapkgs_ui::progress::spinner("Connecting to cache...");
            let result = crate::negotiate::check_server(&server_url).await;
            spinner.finish_and_clear();
            result
        });

        if let Err(e) = server_ok {
            tracing::warn!("Cache unreachable, falling back to nix: {e}");
        } else {
            tracing::info!("Running {installable}");

            let spinner = ekapkgs_ui::progress::spinner("Evaluating closure...");
            let closure_paths = eval::derivation_closure_paths(&inst)?;

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

                let result = rt.block_on(async {
                    let spinner = ekapkgs_ui::progress::spinner("Negotiating with cache...");

                    let response =
                        crate::negotiate::negotiate(&server_url, want_hashes, have_hashes).await?;

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
                });

                if let Err(e) = result {
                    tracing::warn!("Cache negotiation/download failed, falling back to nix: {e}");
                }
            }
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
