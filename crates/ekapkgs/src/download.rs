use std::path::Path;

use ekapkgs_protocol::ekapkgs::v1::{ChunkNegotiateResponse, NegotiateResponse, PathManifestEntry};
use futures::StreamExt;

/// Download NARs according to the negotiate response and import into the nix store.
///
/// Downloads NARs in parallel (per batch), stages them in a temp directory
/// structured as a local binary cache, then imports via `nix copy`.
pub async fn download_and_import(
    server_url: &str,
    response: &NegotiateResponse,
    max_parallel: usize,
) -> color_eyre::Result<()> {
    if response.available.is_empty() {
        return Ok(());
    }

    let staging_dir = tempfile::tempdir()?;
    let nar_dir = staging_dir.path().join("nar");
    std::fs::create_dir_all(&nar_dir)?;

    // Write nix-cache-info so nix recognizes this as a binary cache.
    std::fs::write(
        staging_dir.path().join("nix-cache-info"),
        "StoreDir: /nix/store\n",
    )?;

    let total_paths = response.available.len() as u64;
    let bar = ekapkgs_ui::progress::item_bar(total_paths, "paths");

    // Build a lookup from hash to manifest entry.
    let entry_by_hash: std::collections::HashMap<&str, &PathManifestEntry> = response
        .available
        .iter()
        .filter_map(|e| {
            let hash = e.store_path.rsplit('/').next()?.split('-').next()?;
            Some((hash, e))
        })
        .collect();

    // Process batches in order (dependencies first).
    let plan = response
        .download_plan
        .as_ref()
        .map(|p| &p.batches[..])
        .unwrap_or(&[]);

    let http_client = reqwest::Client::new();
    let base_url = server_url.trim_end_matches('/');

    for batch in plan {
        // Download all paths in this batch in parallel.
        let tasks: Vec<_> = batch
            .paths
            .iter()
            .filter_map(|hash| {
                let entry = *entry_by_hash.get(hash.as_str())?;
                Some((hash.clone(), entry.clone()))
            })
            .collect();

        let results = futures::stream::iter(tasks)
            .map(|(hash, entry)| {
                let client = http_client.clone();
                let base = base_url.to_owned();
                let staging = staging_dir.path().to_path_buf();
                async move {
                    download_single(&client, &base, &entry, &staging).await?;
                    Ok::<String, color_eyre::Report>(hash)
                }
            })
            .buffer_unordered(max_parallel)
            .collect::<Vec<_>>()
            .await;

        for result in results {
            match result {
                Ok(_hash) => bar.inc(1),
                Err(e) => {
                    tracing::warn!("Download failed: {e}");
                },
            }
        }
    }

    // Also handle paths not covered by the download plan.
    let planned: std::collections::HashSet<&str> = plan
        .iter()
        .flat_map(|b| b.paths.iter().map(std::string::String::as_str))
        .collect();

    let unplanned: Vec<_> = response
        .available
        .iter()
        .filter(|e| {
            let hash = e
                .store_path
                .rsplit('/')
                .next()
                .and_then(|b| b.split('-').next())
                .unwrap_or("");
            !planned.contains(hash)
        })
        .collect();

    for entry in unplanned {
        if let Err(e) = download_single(&http_client, base_url, entry, staging_dir.path()).await {
            tracing::warn!("Download failed for {}: {e}", entry.store_path);
        } else {
            bar.inc(1);
        }
    }

    bar.finish_and_clear();

    // Import everything into the nix store.
    tracing::info!("Importing into store...");
    ekapkgs_nix::store::import_from_local_cache(staging_dir.path())?;

    Ok(())
}

/// Download a single NAR and write its narinfo + NAR to the staging directory.
async fn download_single(
    client: &reqwest::Client,
    base_url: &str,
    entry: &PathManifestEntry,
    staging_dir: &Path,
) -> color_eyre::Result<()> {
    let hash = entry
        .store_path
        .rsplit('/')
        .next()
        .and_then(|b| b.split('-').next())
        .ok_or_else(|| color_eyre::eyre::eyre!("invalid store path: {}", entry.store_path))?;

    // Download NAR.
    let nar_url = format!("{base_url}/{}", entry.url);
    let resp = client.get(&nar_url).send().await?;

    if !resp.status().is_success() {
        return Err(color_eyre::eyre::eyre!(
            "NAR download failed: {} {}",
            resp.status(),
            nar_url
        ));
    }

    let nar_bytes = resp.bytes().await?;

    // Write NAR file.
    let nar_path = staging_dir.join(&entry.url);
    if let Some(parent) = nar_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&nar_path, &nar_bytes)?;

    // Write narinfo file.
    let refs: Vec<String> = entry
        .references
        .iter()
        .map(|r| r.rsplit('/').next().unwrap_or(r.as_str()).to_owned())
        .collect();

    let compression = match entry.compression {
        c if c == ekapkgs_protocol::ekapkgs::v1::Compression::Zstd as i32 => "zstd",
        c if c == ekapkgs_protocol::ekapkgs::v1::Compression::Xz as i32 => "xz",
        _ => "none",
    };

    let mut narinfo = String::new();
    narinfo.push_str(&format!("StorePath: {}\n", entry.store_path));
    narinfo.push_str(&format!("URL: {}\n", entry.url));
    narinfo.push_str(&format!("Compression: {compression}\n"));
    if !entry.file_hash.is_empty() {
        narinfo.push_str(&format!("FileHash: {}\n", entry.file_hash));
    }
    if entry.file_size > 0 {
        narinfo.push_str(&format!("FileSize: {}\n", entry.file_size));
    }
    narinfo.push_str(&format!("NarHash: {}\n", entry.nar_hash));
    narinfo.push_str(&format!("NarSize: {}\n", entry.nar_size));
    if !refs.is_empty() {
        narinfo.push_str(&format!("References: {}\n", refs.join(" ")));
    }
    for sig in &entry.signatures {
        narinfo.push_str(&format!("Sig: {sig}\n"));
    }
    if !entry.ca.is_empty() {
        narinfo.push_str(&format!("CA: {}\n", entry.ca));
    }

    let narinfo_path = staging_dir.join(format!("{hash}.narinfo"));
    std::fs::write(&narinfo_path, narinfo)?;

    Ok(())
}

/// Download individual chunks from a CAS-aware server and reassemble NARs
/// for import into the nix store.
///
/// This is the CAS counterpart to `download_and_import`. The server has already
/// told us which chunks are missing via `ChunkNegotiateResponse`. We download
/// them, then rely on the standard NAR download path for actual store import
/// since nix requires NARs for `nix copy`.
pub async fn download_chunks(
    server_url: &str,
    response: &ChunkNegotiateResponse,
    max_parallel: usize,
) -> color_eyre::Result<()> {
    if response.missing_chunks.is_empty() {
        tracing::info!("All chunks already available locally");
        return Ok(());
    }

    let total_chunks = response.missing_chunks.len() as u64;
    let bar = ekapkgs_ui::progress::item_bar(total_chunks, "chunks");

    let http_client = reqwest::Client::new();
    let base_url = server_url.trim_end_matches('/');

    // Download all missing chunks in parallel.
    let chunk_dir = tempfile::tempdir()?;

    let results = futures::stream::iter(response.missing_chunks.iter())
        .map(|chunk| {
            let client = http_client.clone();
            let base = base_url.to_owned();
            let chunk_dir = chunk_dir.path().to_path_buf();
            let url = chunk.url.clone();
            let expected_digest = chunk
                .digest
                .as_ref()
                .map(|d| d.digest.clone())
                .unwrap_or_default();
            async move {
                let chunk_url = format!("{base}/{url}");
                let resp = client.get(&chunk_url).send().await?;

                if !resp.status().is_success() {
                    return Err(color_eyre::eyre::eyre!(
                        "chunk download failed: {} {}",
                        resp.status(),
                        chunk_url
                    ));
                }

                let data = resp.bytes().await?;

                // Verify blake3 digest.
                let actual_hash = blake3::hash(&data);
                if actual_hash.as_bytes() != expected_digest.as_slice() {
                    return Err(color_eyre::eyre::eyre!("chunk digest mismatch for {url}"));
                }

                // Store locally using hex digest as filename.
                let hex: String = expected_digest.iter().map(|b| format!("{b:02x}")).collect();
                let path = chunk_dir.join(format!("{hex}.chunk"));
                std::fs::write(&path, &data)?;

                Ok::<(), color_eyre::Report>(())
            }
        })
        .buffer_unordered(max_parallel)
        .collect::<Vec<_>>()
        .await;

    for result in results {
        match result {
            Ok(()) => bar.inc(1),
            Err(e) => {
                tracing::warn!("Chunk download failed: {e}");
            },
        }
    }

    bar.finish_and_clear();
    tracing::info!(
        "Downloaded {} chunks ({} bytes)",
        total_chunks,
        response.total_chunk_size,
    );

    Ok(())
}
