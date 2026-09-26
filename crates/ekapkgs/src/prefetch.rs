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

        let response =
            crate::negotiate::negotiate(&server_url, want_hashes.clone(), have_hashes.clone())
                .await?;

        spinner.finish_and_clear();

        let avail = response.available.len();
        let unavail = response.unavailable.len();

        if avail > 0 {
            tracing::info!(
                "{avail} paths to download ({} download, {} unpacked)",
                ekapkgs_ui::format::format_bytes(response.total_download_size),
                ekapkgs_ui::format::format_bytes(response.total_nar_size),
            );

            let imported = import_with_fallback(
                &server_url,
                &response,
                want_hashes,
                have_hashes,
                max_parallel,
            )
            .await?;

            tracing::info!("Imported {imported} paths from cache");
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
            want_hashes.clone(),
            have_hashes.clone(),
            target_hash,
        )
        .await?;

        spinner.finish_and_clear();

        let avail = response.available.len();

        if avail > 0 {
            tracing::info!("{avail} paths to download");
            import_with_fallback(
                &server_url,
                &response,
                want_hashes,
                have_hashes,
                max_parallel,
            )
            .await?;
        }

        Ok::<usize, color_eyre::Report>(avail)
    })?;

    Ok(fetched)
}

/// Three-tier import: CAS chunks → gRPC streaming → HTTP batch.
///
/// Returns the number of paths imported.
pub(crate) async fn import_with_fallback(
    server_url: &str,
    response: &ekapkgs_protocol::ekapkgs::v1::NegotiateResponse,
    want_hashes: Vec<String>,
    have_hashes: Vec<String>,
    max_parallel: usize,
) -> color_eyre::Result<usize> {
    let avail = response.available.len();
    let mut imported = false;

    // Tier 1: CAS chunk-based pull.
    if !response.ca_path_mappings.is_empty() {
        match crate::cas_pull::cas_pull(
            server_url,
            want_hashes,
            have_hashes,
            &response.available,
            max_parallel,
        )
        .await
        {
            Ok(true) => {
                imported = true;
            },
            Ok(false) => {
                tracing::debug!("CAS pull not available, falling back");
            },
            Err(e) => {
                tracing::warn!("CAS pull failed: {e}, falling back");
            },
        }
    }

    // Tier 2: gRPC streaming.
    if !imported {
        match crate::download::stream_and_import(server_url, response).await {
            Ok(()) => {
                imported = true;
            },
            Err(e) => {
                let is_unimplemented = e
                    .downcast_ref::<tonic::Status>()
                    .is_some_and(|s| s.code() == tonic::Code::Unimplemented);
                if !is_unimplemented {
                    return Err(e);
                }
            },
        }
    }

    // Tier 3: HTTP batch download.
    if !imported {
        crate::download::download_and_import(server_url, response, max_parallel).await?;
    }

    Ok(avail)
}
