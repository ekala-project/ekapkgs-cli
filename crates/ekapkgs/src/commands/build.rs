use ekapkgs_nix::installable::Installable;
use ekapkgs_nix::{NixCommand, NixError, eval, store};

use crate::config::ClientConfig;

/// Execute `ekapkgs build`.
///
/// If an ekapkgs cache is configured, uses the negotiation protocol to
/// download missing closure paths before building. Otherwise, falls back
/// to standard nix substitution.
pub fn execute(installable: &str, extra: &[String]) -> color_eyre::Result<()> {
    let config = ClientConfig::load()?;
    let inst = Installable::new(installable);

    // If we have a configured cache, try negotiated substitution.
    if let Some(cache) = config.primary_cache() {
        tracing::info!("Building {installable}");

        let spinner = ekapkgs_ui::progress::spinner("Evaluating closure...");

        // Get the full closure of output paths.
        let closure_paths = eval::derivation_closure_paths(&inst)?;

        spinner.set_message("Checking local store...");

        // Partition into what we have and what we need.
        let (have, want) = store::partition_local(&closure_paths)?;

        spinner.finish_and_clear();

        if want.is_empty() {
            tracing::info!("All {} paths already in local store", have.len());
        } else {
            tracing::info!(
                "Closure: {} paths ({} in local store, {} to fetch)",
                closure_paths.len(),
                have.len(),
                want.len()
            );

            // Convert store paths to hashes for the negotiate request.
            let want_hashes: Vec<String> = want
                .iter()
                .filter_map(|p| store::store_path_hash(p).map(String::from))
                .collect();
            let have_hashes: Vec<String> = have
                .iter()
                .filter_map(|p| store::store_path_hash(p).map(String::from))
                .collect();

            let server_url = cache.url.clone();

            // Run the async negotiation + download.
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                let spinner = ekapkgs_ui::progress::spinner("Negotiating with cache...");

                let response =
                    crate::negotiate::negotiate(&server_url, want_hashes, have_hashes).await?;

                spinner.finish_and_clear();

                let avail = response.available.len();
                let unavail = response.unavailable.len();
                let dl_size = response.total_download_size;
                let nar_size = response.total_nar_size;

                if avail > 0 {
                    tracing::info!(
                        "{avail} paths to download ({} download, {} unpacked)",
                        format_bytes(dl_size),
                        format_bytes(nar_size),
                    );

                    crate::download::download_and_import(
                        &server_url,
                        &response,
                        config.defaults.max_parallel_downloads,
                    )
                    .await?;

                    tracing::info!("Imported {avail} paths from cache");
                }

                if unavail > 0 {
                    tracing::info!("{unavail} paths not on cache, falling back to nix");
                }

                Ok::<(), color_eyre::Report>(())
            })?;
        }
    } else {
        tracing::info!("Building {installable} (no ekapkgs cache configured)");
    }

    // Final build — nix handles any remaining substitution.
    let mut cmd = NixCommand::new(&["build"]).arg(installable);
    for arg in extra {
        cmd = cmd.arg(arg);
    }
    match cmd.stream_with_monitor() {
        Ok(_) => {
            tracing::info!("Build complete");
            Ok(())
        },
        Err(NixError::Failed { status, .. }) => {
            // Nix's stderr was already printed by stream_with_monitor.
            // Exit with nix's exit code without additional ekapkgs error output.
            std::process::exit(status.code().unwrap_or(1));
        },
        Err(e) => Err(e.into()),
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;

    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}
