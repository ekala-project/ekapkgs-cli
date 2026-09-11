//! CAS (content-addressed store) pull orchestration.
//!
//! Negotiates at the chunk level with the server, downloads only missing
//! chunks, reassembles NARs locally, and imports into the nix store.

use std::collections::HashMap;

use ekapkgs_nix::nar::write_nar;
use ekapkgs_protocol::ekapkgs::v1::PathManifestEntry;
use futures::StreamExt;

use crate::chunk_store::ChunkStore;

/// Attempt a CAS-based pull for the given paths.
///
/// Returns `Ok(true)` if the CAS pull succeeded and all paths were imported.
/// Returns `Ok(false)` if the server does not support chunk-level negotiation
/// (the caller should fall back to NAR streaming or HTTP downloads).
/// Returns `Err` on actual failures.
pub async fn cas_pull(
    server_url: &str,
    want_hashes: Vec<String>,
    have_hashes: Vec<String>,
    available: &[PathManifestEntry],
    max_parallel: usize,
) -> color_eyre::Result<bool> {
    // 1. Open the client-side chunk store.
    let store = ChunkStore::open()?;

    // 2. Gather local chunk digests for the negotiation request.
    let have_chunks = store.all_chunk_digests()?;
    let have_count = have_chunks.len();
    if have_count > 0 {
        tracing::debug!("Local chunk store has {have_count} chunks");
    }

    // 3. Negotiate chunks with the server.
    let response =
        match crate::negotiate::negotiate_chunks(server_url, want_hashes, have_hashes, have_chunks)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                // If the server doesn't support NegotiateChunks, signal fallback.
                if let Some(status) = e.downcast_ref::<tonic::Status>() {
                    if status.code() == tonic::Code::Unimplemented {
                        return Ok(false);
                    }
                }
                return Err(e);
            },
        };

    if response.path_mappings.is_empty() {
        // Server returned no CAS data — nothing to do via CAS.
        return Ok(false);
    }

    // 4. Index directory and file-chunk metadata from the response.
    store.index_response(&response)?;

    // 5. Download missing chunks in parallel.
    let missing_count = response.missing_chunks.len();
    if missing_count > 0 {
        tracing::info!(
            "Downloading {missing_count} chunks ({} total)",
            ekapkgs_ui::format::format_bytes(response.total_chunk_size),
        );

        let bar = ekapkgs_ui::progress::download_bar(response.total_chunk_size);
        let http_client = reqwest::Client::new();
        let base_url = server_url.trim_end_matches('/');

        let results = futures::stream::iter(response.missing_chunks.iter())
            .map(|chunk| {
                let client = http_client.clone();
                let base = base_url.to_owned();
                let url = chunk.url.clone();
                let size = chunk.size;
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
                            chunk_url,
                        ));
                    }

                    let data = resp.bytes().await?;

                    // Verify blake3 digest.
                    let actual_hash = blake3::hash(&data);
                    if actual_hash.as_bytes() != expected_digest.as_slice() {
                        return Err(color_eyre::eyre::eyre!("chunk digest mismatch for {url}"));
                    }

                    let digest: [u8; 32] = expected_digest
                        .as_slice()
                        .try_into()
                        .map_err(|_| color_eyre::eyre::eyre!("invalid digest length"))?;

                    Ok::<_, color_eyre::Report>((digest, data.to_vec(), size))
                }
            })
            .buffer_unordered(max_parallel)
            .collect::<Vec<_>>()
            .await;

        for result in results {
            match result {
                Ok((digest, data, size)) => {
                    store.store_chunk(&digest, &data)?;
                    bar.inc(size);
                },
                Err(e) => {
                    tracing::warn!("Chunk download failed: {e}");
                },
            }
        }

        bar.finish_and_clear();
    } else {
        tracing::info!("All chunks already available locally");
    }

    // 6. Reassemble NARs and stage for import.
    let staging_dir = tempfile::tempdir()?;
    let nar_dir = staging_dir.path().join("nar");
    std::fs::create_dir_all(&nar_dir)?;

    std::fs::write(
        staging_dir.path().join("nix-cache-info"),
        "StoreDir: /nix/store\n",
    )?;

    let entry_by_hash: HashMap<&str, &PathManifestEntry> = available
        .iter()
        .filter_map(|e| {
            let hash = e.store_path.rsplit('/').next()?.split('-').next()?;
            Some((hash, e))
        })
        .collect();

    let mut reassembled = 0u64;
    let reassembly_bar =
        ekapkgs_ui::progress::item_bar(response.path_mappings.len() as u64, "paths");

    for mapping in &response.path_mappings {
        let hash = &mapping.store_path_hash;
        let Some(root_node) = &mapping.root_node else {
            reassembly_bar.inc(1);
            continue;
        };
        let Some(entry) = entry_by_hash.get(hash.as_str()) else {
            tracing::warn!("No manifest entry for CAS path {hash}, skipping");
            reassembly_bar.inc(1);
            continue;
        };

        // Reconstruct the NarNode tree from the CaNode tree.
        let nar_node = match store.reconstruct_node(root_node) {
            Ok(node) => node,
            Err(e) => {
                tracing::warn!("NAR reassembly failed for {hash}: {e}");
                reassembly_bar.inc(1);
                continue;
            },
        };

        // Serialize to NAR bytes.
        let nar_bytes = write_nar(&nar_node);

        // Write NAR file to staging.
        let nar_path = staging_dir.path().join(&entry.url);
        if let Some(parent) = nar_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&nar_path, &nar_bytes)?;

        // Write narinfo.
        let narinfo_text = crate::download::build_narinfo_text(entry);
        let narinfo_path = staging_dir.path().join(format!("{hash}.narinfo"));
        std::fs::write(&narinfo_path, narinfo_text)?;

        reassembled += 1;
        reassembly_bar.inc(1);
    }

    reassembly_bar.finish_and_clear();

    if reassembled == 0 {
        return Ok(false);
    }

    // 7. Import into the nix store.
    tracing::info!("Importing {reassembled} paths into store...");
    ekapkgs_nix::store::import_from_local_cache(staging_dir.path())?;

    // 8. Record CAS path mappings for future chunk inventory.
    for mapping in &response.path_mappings {
        if let Some(root_node) = &mapping.root_node {
            let _ = store.record_cas_path(&mapping.store_path_hash, root_node);
        }
    }

    Ok(true)
}
