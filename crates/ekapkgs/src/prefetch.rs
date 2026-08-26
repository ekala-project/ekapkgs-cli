use ekapkgs_nix::store;

use crate::config::ClientConfig;

/// Pre-fetch missing closure paths from an ekapkgs cache.
///
/// Given a set of closure store paths, negotiates with the configured cache
/// and downloads any available paths. Returns the count of fetched paths.
///
/// If no cache is configured, returns 0 without error.
pub fn prefetch_closure(
    config: &ClientConfig,
    closure_paths: &[String],
) -> color_eyre::Result<usize> {
    let Some(cache) = config.primary_cache() else {
        return Ok(0);
    };

    let (have, want) = store::partition_local(closure_paths)?;

    if want.is_empty() {
        tracing::info!("All {} paths already in local store", have.len());
        return Ok(0);
    }

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
    let max_parallel = config.defaults.max_parallel_downloads;

    let rt = tokio::runtime::Runtime::new()?;
    let fetched = rt.block_on(async {
        let spinner = ekapkgs_ui::progress::spinner("Negotiating with cache...");

        let response = crate::negotiate::negotiate(&server_url, want_hashes, have_hashes).await?;

        spinner.finish_and_clear();

        let avail = response.available.len();
        let unavail = response.unavailable.len();

        if avail > 0 {
            tracing::info!(
                "{avail} paths to download ({} download, {} unpacked)",
                ekapkgs_ui::format::format_bytes(response.total_download_size),
                ekapkgs_ui::format::format_bytes(response.total_nar_size),
            );

            crate::download::download_and_import(&server_url, &response, max_parallel).await?;

            tracing::info!("Imported {avail} paths from cache");
        }

        if unavail > 0 {
            tracing::info!("{unavail} paths not on cache, falling back to nix");
        }

        Ok::<usize, color_eyre::Report>(avail)
    })?;

    Ok(fetched)
}

/// Pre-fetch with critical-path prioritization for a target hash.
///
/// The server prioritizes the target and its transitive runtime dependencies
/// in the download plan.
#[allow(dead_code)]
pub fn prefetch_closure_with_target(
    config: &ClientConfig,
    closure_paths: &[String],
    target_hash: Option<&str>,
) -> color_eyre::Result<usize> {
    let Some(cache) = config.primary_cache() else {
        return Ok(0);
    };

    let (have, want) = store::partition_local(closure_paths)?;

    if want.is_empty() {
        return Ok(0);
    }

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
    let max_parallel = config.defaults.max_parallel_downloads;

    let rt = tokio::runtime::Runtime::new()?;
    let fetched = rt.block_on(async {
        let spinner = ekapkgs_ui::progress::spinner("Negotiating with cache...");

        let response = crate::negotiate::negotiate_with_target(
            &server_url,
            want_hashes,
            have_hashes,
            target_hash,
        )
        .await?;

        spinner.finish_and_clear();

        let avail = response.available.len();

        if avail > 0 {
            tracing::info!("{avail} paths to download");
            crate::download::download_and_import(&server_url, &response, max_parallel).await?;
        }

        Ok::<usize, color_eyre::Report>(avail)
    })?;

    Ok(fetched)
}
