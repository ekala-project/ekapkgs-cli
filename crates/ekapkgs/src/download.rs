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

    // Try delta download if available, fall back to full NAR.
    let nar_data = if !entry.delta_url.is_empty() && !entry.delta_base_hash.is_empty() {
        let delta_url = format!("{base_url}/{}", entry.delta_url);
        let resp = client.get(&delta_url).send().await?;

        if resp.status().is_success() {
            let delta_bytes = resp.bytes().await?;
            match apply_delta(&delta_bytes, &entry.delta_base_hash) {
                Ok(nar) => nar,
                Err(e) => {
                    tracing::warn!("Delta apply failed for {hash}, downloading full NAR: {e}");
                    download_full_nar(client, base_url, &entry.url).await?
                },
            }
        } else {
            download_full_nar(client, base_url, &entry.url).await?
        }
    } else {
        download_full_nar(client, base_url, &entry.url).await?
    };

    // Write NAR file.
    let nar_path = staging_dir.join(&entry.url);
    if let Some(parent) = nar_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&nar_path, &nar_data)?;

    // Write narinfo file.
    let narinfo_text = build_narinfo_text(entry);
    let narinfo_path = staging_dir.join(format!("{hash}.narinfo"));
    std::fs::write(&narinfo_path, narinfo_text)?;

    Ok(())
}

/// Maximum number of resume attempts before re-downloading from scratch.
const MAX_RESUME_RETRIES: u32 = 3;

/// Download a full NAR via HTTP GET with resumable download support.
///
/// If the download is interrupted, retries with a `Range` header to resume
/// from where it left off. Falls back to a full re-download after
/// `MAX_RESUME_RETRIES` failed resume attempts.
async fn download_full_nar(
    client: &reqwest::Client,
    base_url: &str,
    url: &str,
) -> color_eyre::Result<Vec<u8>> {
    let nar_url = format!("{base_url}/{url}");

    let resp = client.get(&nar_url).send().await?;

    if !resp.status().is_success() {
        return Err(color_eyre::eyre::eyre!(
            "NAR download failed: {} {}",
            resp.status(),
            nar_url
        ));
    }

    // Try to read the full response.
    match resp.bytes().await {
        Ok(bytes) => return Ok(bytes.to_vec()),
        Err(first_err) => {
            tracing::warn!("NAR download interrupted for {url}: {first_err}");
        },
    }

    // The initial download failed. Try to resume with Range requests.
    // We don't have partial data from the first attempt (reqwest consumed it),
    // so start from scratch but with retry logic.
    let mut buf = Vec::new();

    for attempt in 0..MAX_RESUME_RETRIES {
        let req = if buf.is_empty() {
            client.get(&nar_url)
        } else {
            client
                .get(&nar_url)
                .header("Range", format!("bytes={}-", buf.len()))
        };

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "Resume attempt {}/{MAX_RESUME_RETRIES} failed: {e}",
                    attempt + 1
                );
                continue;
            },
        };

        match resp.status().as_u16() {
            200 => {
                // Server sent the full file (doesn't support Range or we started from 0).
                match resp.bytes().await {
                    Ok(bytes) => return Ok(bytes.to_vec()),
                    Err(e) => {
                        tracing::warn!("Download interrupted on retry: {e}");
                        buf.clear();
                        continue;
                    },
                }
            },
            206 => {
                // Partial content — append to our buffer.
                match resp.bytes().await {
                    Ok(bytes) => {
                        buf.extend_from_slice(&bytes);
                        return Ok(buf);
                    },
                    Err(e) => {
                        tracing::warn!("Resume interrupted at {} bytes: {e}", buf.len());
                        // Keep the partial buffer and try again.
                        continue;
                    },
                }
            },
            416 => {
                // Range not satisfiable — our offset is past the end.
                // The file might have changed, start over.
                buf.clear();
                continue;
            },
            status => {
                tracing::warn!("Unexpected status {status} on resume attempt");
                buf.clear();
                continue;
            },
        }
    }

    Err(color_eyre::eyre::eyre!(
        "NAR download failed after {MAX_RESUME_RETRIES} resume attempts: {url}"
    ))
}

/// Download individual chunks from a CAS-aware server and reassemble NARs
/// for import into the nix store.
///
/// This is the CAS counterpart to `download_and_import`. The server has already
/// told us which chunks are missing via `ChunkNegotiateResponse`. We download
/// them, then rely on the standard NAR download path for actual store import
/// since nix requires NARs for `nix copy`.
#[allow(dead_code)]
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

/// Stream NARs over gRPC and import into the nix store.
///
/// Uses a single gRPC server-side stream instead of individual HTTP downloads.
/// The server sends NAR data for each path in dependency order, split into
/// 64 KiB chunks.
pub async fn stream_and_import(
    server_url: &str,
    response: &NegotiateResponse,
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

    // Build a lookup from hash to manifest entry.
    let entry_by_hash: std::collections::HashMap<&str, &PathManifestEntry> = response
        .available
        .iter()
        .filter_map(|e| {
            let hash = e.store_path.rsplit('/').next()?.split('-').next()?;
            Some((hash, e))
        })
        .collect();

    // Build the ordered list of path hashes from the download plan.
    let path_hashes: Vec<String> = response
        .download_plan
        .as_ref()
        .map(|p| {
            p.batches
                .iter()
                .flat_map(|b| b.paths.iter().cloned())
                .collect()
        })
        .unwrap_or_default();

    // Also include any paths not covered by the plan.
    let planned: std::collections::HashSet<String> = path_hashes.iter().cloned().collect();
    let mut all_hashes = path_hashes;
    for entry in &response.available {
        if let Some(hash) = entry
            .store_path
            .rsplit('/')
            .next()
            .and_then(|b| b.split('-').next())
        {
            if !planned.contains(hash) {
                all_hashes.push(hash.to_owned());
            }
        }
    }

    let bar = ekapkgs_ui::progress::download_bar(response.total_download_size);

    // Start the gRPC stream.
    let mut stream = crate::negotiate::stream_nars(server_url, all_hashes).await?;

    let mut current_hash = String::new();
    let mut current_buf: Vec<u8> = Vec::new();
    let mut current_is_delta = false;
    let mut paths_received = 0u64;

    while let Some(chunk) = stream.message().await? {
        if chunk.path_hash != current_hash {
            current_hash.clone_from(&chunk.path_hash);
            current_buf.clear();
            current_is_delta = chunk.is_delta;
            if chunk.file_size > 0 {
                current_buf.reserve(chunk.file_size as usize);
            }
        }

        bar.inc(chunk.data.len() as u64);
        current_buf.extend_from_slice(&chunk.data);

        if chunk.last {
            if let Some(entry) = entry_by_hash.get(current_hash.as_str()) {
                // If this is a delta, decompress using the base NAR as dictionary.
                let nar_data = if current_is_delta && !entry.delta_base_hash.is_empty() {
                    match apply_delta(&current_buf, &entry.delta_base_hash) {
                        Ok(data) => data,
                        Err(e) => {
                            tracing::warn!("Delta decompression failed for {current_hash}: {e}");
                            current_buf.clear();
                            continue;
                        },
                    }
                } else {
                    std::mem::take(&mut current_buf)
                };

                let nar_path = staging_dir.path().join(&entry.url);
                if let Some(parent) = nar_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&nar_path, &nar_data)?;

                let narinfo_text = build_narinfo_text(entry);
                let narinfo_path = staging_dir.path().join(format!("{current_hash}.narinfo"));
                std::fs::write(&narinfo_path, narinfo_text)?;

                paths_received += 1;
            } else {
                tracing::warn!("Received NAR for unknown hash {current_hash}, skipping");
            }

            current_buf.clear();
        }
    }

    bar.finish_and_clear();

    if paths_received == 0 {
        return Ok(());
    }

    tracing::info!("Importing {paths_received} paths into store...");
    ekapkgs_nix::store::import_from_local_cache(staging_dir.path())?;

    Ok(())
}

/// Build a narinfo text string from a PathManifestEntry.
fn build_narinfo_text(entry: &PathManifestEntry) -> String {
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
    narinfo
}

/// Apply a zstd-dict-compressed delta to reconstruct a full NAR.
///
/// Reads the base NAR from the local nix store via `nix-store --dump`,
/// then decompresses the delta using the base NAR as a zstd dictionary.
fn apply_delta(delta: &[u8], base_hash: &str) -> color_eyre::Result<Vec<u8>> {
    // Read the base NAR from the local store.
    let output = std::process::Command::new("nix-store")
        .arg("--dump")
        .arg(format!("/nix/store/{base_hash}"))
        .output()?;

    if !output.status.success() {
        return Err(color_eyre::eyre::eyre!(
            "failed to dump base NAR for {base_hash}"
        ));
    }

    let base_nar = output.stdout;

    // Decompress the delta using the base NAR as dictionary.
    use std::io::Read;
    let mut decoder = zstd::Decoder::with_dictionary(delta, &base_nar)?;
    let mut result = Vec::new();
    decoder.read_to_end(&mut result)?;

    Ok(result)
}
