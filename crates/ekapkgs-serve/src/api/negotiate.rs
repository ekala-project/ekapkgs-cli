use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ekapkgs_protocol::ekapkgs::v1::cache_service_server::CacheService;
use ekapkgs_protocol::ekapkgs::v1::{
    CaPathMapping, ChunkDownload, ChunkNegotiateRequest, ChunkNegotiateResponse, Compression,
    DownloadBatch, DownloadPlan, NarChunk, NegotiateRequest, NegotiateResponse, PathManifestEntry,
    StreamNarsRequest,
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

/// Maximum size of each NarChunk payload (64 KiB).
const NAR_CHUNK_SIZE: usize = 65_536;

use crate::AppState;

pub struct NegotiateService {
    pub state: Arc<AppState>,
}

#[tonic::async_trait]
impl CacheService for NegotiateService {
    type StreamNarsStream = ReceiverStream<Result<NarChunk, Status>>;

    async fn negotiate(
        &self,
        request: Request<NegotiateRequest>,
    ) -> Result<Response<NegotiateResponse>, Status> {
        let req = request.into_inner();

        self.state.metrics.negotiate_requests_total.inc();
        self.state
            .metrics
            .negotiate_paths_requested
            .with_label_values(&[])
            .observe(req.want.len() as f64);

        let have_set: HashSet<&str> = req.have.iter().map(std::string::String::as_str).collect();

        let mut available = Vec::new();
        let mut unavailable = Vec::new();
        let mut total_download_size: u64 = 0;
        let mut total_nar_size: u64 = 0;

        // Query storage for each wanted path.
        for hash in &req.want {
            match self.state.storage.get_narinfo(hash) {
                Ok(Some(mut ni)) => {
                    // Re-sign with our key.
                    let fingerprint = crate::signing::NarInfoSigner::fingerprint(
                        &ni.store_path,
                        &ni.nar_hash,
                        ni.nar_size,
                        &ni.references,
                    );
                    let sig = self.state.signer.sign(&fingerprint);
                    if !ni.signatures.contains(&sig) {
                        ni.signatures.push(sig);
                    }

                    let compression = match ni.compression.as_str() {
                        "zstd" => Compression::Zstd as i32,
                        "xz" => Compression::Xz as i32,
                        "none" | "" => Compression::None as i32,
                        _ => Compression::Unspecified as i32,
                    };

                    total_download_size += ni.file_size;
                    total_nar_size += ni.nar_size;

                    available.push(PathManifestEntry {
                        store_path: ni.store_path,
                        nar_hash: ni.nar_hash,
                        nar_size: ni.nar_size,
                        references: ni.references,
                        signatures: ni.signatures,
                        cert_signature: None,
                        ca: ni.ca.unwrap_or_default(),
                        url: ni.url,
                        compression,
                        file_hash: ni.file_hash,
                        file_size: ni.file_size,
                        delta_base_hash: String::new(),
                        delta_url: String::new(),
                        delta_size: 0,
                    });
                },
                Ok(None) => {
                    unavailable.push(hash.clone());
                },
                Err(e) => {
                    tracing::warn!("Failed to query narinfo for {hash}: {e}");
                    unavailable.push(hash.clone());
                },
            }
        }

        // Apply certificate-based signatures if configured.
        let certificate_chain = if let Some(ref cert_signer) = self.state.cert_signer {
            for entry in &mut available {
                let fingerprint = crate::signing::NarInfoSigner::fingerprint(
                    &entry.store_path,
                    &entry.nar_hash,
                    entry.nar_size,
                    &entry.references,
                );
                entry.cert_signature = Some(cert_signer.sign(&fingerprint));
            }
            Some(cert_signer.chain.clone())
        } else {
            None
        };

        // Record accesses for GC tracking.
        if let Some(ref tracker) = self.state.gc_tracker {
            for entry in &available {
                if let Some(hash) = entry
                    .store_path
                    .rsplit('/')
                    .next()
                    .and_then(|b| b.split('-').next())
                {
                    tracker.record_access(hash);
                }
            }
        }

        // Build download plan: topological sort by references.
        let download_plan = build_download_plan(&available, &have_set);

        // If client supports CAS and backend has CAS data, include path mappings.
        let ca_path_mappings = if req.supports_cas && self.state.storage.supports_cas() {
            available
                .iter()
                .filter_map(|entry| {
                    let hash = entry
                        .store_path
                        .rsplit('/')
                        .next()
                        .and_then(|b| b.split('-').next())?;
                    let root_bytes = self.state.storage.get_cas_root(hash).ok()??;
                    let root_node = prost::Message::decode(root_bytes.as_slice()).ok()?;
                    Some(CaPathMapping {
                        store_path_hash: hash.to_owned(),
                        root_node: Some(root_node),
                    })
                })
                .collect()
        } else {
            Vec::new()
        };

        self.state
            .metrics
            .negotiate_paths_available
            .with_label_values(&[])
            .observe(available.len() as f64);

        // Compute delta transfers: for each available path, check if the client
        // has an older version of the same package that can serve as a delta base.
        compute_deltas(
            &self.state,
            &req.have,
            &mut available,
            &mut total_download_size,
        );

        Ok(Response::new(NegotiateResponse {
            available,
            unavailable,
            certificate_chain,
            download_plan: Some(download_plan),
            total_download_size,
            total_nar_size,
            ca_path_mappings,
        }))
    }

    async fn negotiate_chunks(
        &self,
        request: Request<ChunkNegotiateRequest>,
    ) -> Result<Response<ChunkNegotiateResponse>, Status> {
        if !self.state.storage.supports_cas() {
            return Err(Status::unimplemented("CAS storage not configured"));
        }

        let req = request.into_inner();

        // Build the set of chunk digests the client already has.
        let have_digests: std::collections::HashSet<[u8; 32]> = req
            .have_chunks
            .iter()
            .filter_map(|d| d.digest.as_slice().try_into().ok())
            .collect();

        let mut path_mappings = Vec::new();
        let mut unavailable = Vec::new();

        // Collect root nodes for each wanted path.
        let mut want_hashes = Vec::new();
        for hash in &req.want {
            match self.state.storage.get_cas_root(hash) {
                Ok(Some(root_bytes)) => {
                    if let Ok(root_node) = prost::Message::decode(root_bytes.as_slice()) {
                        path_mappings.push(CaPathMapping {
                            store_path_hash: hash.clone(),
                            root_node: Some(root_node),
                        });
                        want_hashes.push(hash.as_str());
                    } else {
                        unavailable.push(hash.clone());
                    }
                },
                Ok(None) => unavailable.push(hash.clone()),
                Err(e) => {
                    tracing::warn!("Failed to query CAS root for {hash}: {e}");
                    unavailable.push(hash.clone());
                },
            }
        }

        // Walk the Merkle trees to find missing chunks. We need to downcast to
        // CastoreBackend for the walk_missing_chunks method.
        let missing_chunks = if let Some(castore) =
            self.state
                .storage
                .as_any()
                .downcast_ref::<crate::storage::castore::CastoreBackend>()
        {
            let chunks = castore
                .walk_missing_chunks(&want_hashes, &have_digests)
                .map_err(|e| Status::internal(format!("chunk walk failed: {e}")))?;

            chunks
                .into_iter()
                .map(|cm| {
                    let hex = cm
                        .digest
                        .as_ref()
                        .map(|d| {
                            d.digest
                                .iter()
                                .map(|b| format!("{b:02x}"))
                                .collect::<String>()
                        })
                        .unwrap_or_default();
                    ChunkDownload {
                        digest: cm.digest,
                        size: cm.size,
                        url: format!("cas/chunk/{hex}"),
                    }
                })
                .collect()
        } else {
            Vec::new()
        };

        let total_chunk_size: u64 = missing_chunks.iter().map(|c| c.size).sum();

        Ok(Response::new(ChunkNegotiateResponse {
            path_mappings,
            missing_chunks,
            unavailable,
            total_chunk_size,
        }))
    }

    async fn stream_nars(
        &self,
        request: Request<StreamNarsRequest>,
    ) -> Result<Response<Self::StreamNarsStream>, Status> {
        let req = request.into_inner();
        let (tx, rx) = tokio::sync::mpsc::channel(16);

        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            for hash in req.path_hashes {
                // Look up narinfo to get the NAR URL and file size.
                let narinfo = match state.storage.get_narinfo(&hash) {
                    Ok(Some(ni)) => ni,
                    Ok(None) => {
                        tracing::warn!("StreamNars: path {hash} not found, skipping");
                        continue;
                    },
                    Err(e) => {
                        tracing::warn!("StreamNars: failed to query {hash}: {e}");
                        continue;
                    },
                };

                // Check if a delta is cached for this path.
                let (stream_data, is_delta) = {
                    // Try all possible base hashes in the delta cache.
                    let delta = state.delta_cache.get_for_target(&hash);
                    if let Some(d) = delta {
                        (d, true)
                    } else {
                        // Load the full NAR.
                        match state.storage.get_nar(&narinfo.url) {
                            Ok(Some(data)) => (data, false),
                            Ok(None) => {
                                tracing::warn!("StreamNars: NAR not found for {hash}, skipping");
                                continue;
                            },
                            Err(e) => {
                                tracing::warn!("StreamNars: failed to load NAR for {hash}: {e}");
                                continue;
                            },
                        }
                    }
                };

                // Record GC access.
                if let Some(ref tracker) = state.gc_tracker {
                    tracker.record_access(&hash);
                }

                // Split into chunks and stream.
                let total = stream_data.len();
                let mut offset = 0;
                let mut first = true;

                while offset < total {
                    let end = (offset + NAR_CHUNK_SIZE).min(total);
                    let last = end == total;

                    let chunk = NarChunk {
                        path_hash: hash.clone(),
                        data: stream_data[offset..end].to_vec(),
                        last,
                        file_size: if first { total as u64 } else { 0 },
                        is_delta,
                    };
                    first = false;
                    offset = end;

                    if tx.send(Ok(chunk)).await.is_err() {
                        return;
                    }
                }

                if total == 0 {
                    let chunk = NarChunk {
                        path_hash: hash.clone(),
                        data: Vec::new(),
                        last: true,
                        file_size: 0,
                        is_delta,
                    };
                    if tx.send(Ok(chunk)).await.is_err() {
                        return;
                    }
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

/// Build a topologically-sorted download plan.
///
/// Paths are grouped into batches. Within each batch, paths can be downloaded
/// in parallel. Batches must be processed in order (dependencies before
/// dependents).
fn build_download_plan(entries: &[PathManifestEntry], have: &HashSet<&str>) -> DownloadPlan {
    // Track which paths have been assigned to a batch.
    let mut assigned: HashSet<usize> = HashSet::new();
    // Track which hashes are "resolved" (available for dependents).
    let mut resolved: HashSet<String> = have.iter().map(|s| (*s).to_owned()).collect();

    let mut batches = Vec::new();

    // Iteratively find paths whose references are all resolved.
    loop {
        let mut batch_paths = Vec::new();

        for (i, entry) in entries.iter().enumerate() {
            if assigned.contains(&i) {
                continue;
            }

            // Check if all references are resolved.
            let all_resolved = entry.references.iter().all(|r| {
                let ref_hash = r.rsplit('/').next().and_then(|b| b.split('-').next());
                match ref_hash {
                    Some(h) => resolved.contains(h),
                    None => true,
                }
            });

            if all_resolved {
                let hash = entry
                    .store_path
                    .rsplit('/')
                    .next()
                    .and_then(|b| b.split('-').next())
                    .unwrap_or("")
                    .to_owned();
                batch_paths.push(hash);
                assigned.insert(i);
            }
        }

        if batch_paths.is_empty() {
            // Remaining paths have circular deps or missing refs — dump them all.
            for (i, entry) in entries.iter().enumerate() {
                if !assigned.contains(&i) {
                    let hash = entry
                        .store_path
                        .rsplit('/')
                        .next()
                        .and_then(|b| b.split('-').next())
                        .unwrap_or("")
                        .to_owned();
                    batch_paths.push(hash);
                }
            }
            if !batch_paths.is_empty() {
                batches.push(DownloadBatch {
                    paths: batch_paths,
                    priority: batches.len() as u32,
                });
            }
            break;
        }

        // Mark batch paths as resolved for the next iteration.
        for h in &batch_paths {
            resolved.insert(h.clone());
        }

        batches.push(DownloadBatch {
            paths: batch_paths,
            priority: batches.len() as u32,
        });

        if assigned.len() == entries.len() {
            break;
        }
    }

    DownloadPlan { batches }
}

/// Maximum delta-to-full ratio: only offer a delta if it's smaller than this
/// fraction of the full NAR size.
const DELTA_MAX_RATIO: f64 = 0.80;

/// Scan available paths for delta transfer opportunities against the client's
/// `have` set. For each match, compress the new NAR using the old NAR as a zstd
/// dictionary and store the result in the delta cache.
fn compute_deltas(
    state: &crate::AppState,
    have_hashes: &[String],
    available: &mut [PathManifestEntry],
    total_download_size: &mut u64,
) {
    // Build a map of pname → (hash, store_path) from the client's `have` set.
    let mut have_by_pname: HashMap<String, (String, String)> = HashMap::new();
    for hash in have_hashes {
        if let Ok(Some(ni)) = state.storage.get_narinfo(hash) {
            if let Some(pname) = extract_pname(&ni.store_path) {
                have_by_pname.insert(pname, (hash.clone(), ni.store_path.clone()));
            }
        }
    }

    if have_by_pname.is_empty() {
        return;
    }

    for entry in available.iter_mut() {
        let Some(target_pname) = extract_pname(&entry.store_path) else {
            continue;
        };

        let Some((base_hash, _base_store_path)) = have_by_pname.get(&target_pname) else {
            continue;
        };

        let target_hash = entry
            .store_path
            .rsplit('/')
            .next()
            .and_then(|b| b.split('-').next())
            .unwrap_or("")
            .to_owned();

        // Don't delta against ourselves.
        if *base_hash == target_hash {
            continue;
        }

        // Load both NARs.
        let Ok(Some(base_nar)) = state.storage.get_nar(&format!("nar/{base_hash}.nar")) else {
            continue;
        };
        let Ok(Some(target_nar)) = state.storage.get_nar(&entry.url) else {
            continue;
        };

        // Compress the target NAR using the base NAR as a zstd dictionary.
        let Some(delta) = compress_with_dict(&target_nar, &base_nar) else {
            continue;
        };

        // Only offer the delta if it's meaningfully smaller.
        if !target_nar.is_empty()
            && (delta.len() as f64 / target_nar.len() as f64) < DELTA_MAX_RATIO
        {
            // Update download size tracking.
            *total_download_size = total_download_size.saturating_sub(entry.file_size);
            *total_download_size += delta.len() as u64;

            entry.delta_base_hash = base_hash.clone();
            entry.delta_url = format!("delta/{base_hash}/{target_hash}");
            entry.delta_size = delta.len() as u64;

            state
                .delta_cache
                .insert(base_hash.clone(), target_hash, delta);

            tracing::debug!(
                "Delta available for {}: {} -> {} bytes (base: {base_hash})",
                entry.store_path,
                entry.file_size,
                entry.delta_size,
            );
        }
    }
}

/// Extract the package name from a nix store path, stripping the hash prefix
/// and version suffix.
///
/// `/nix/store/abc123-firefox-131.0` → `"firefox"`
/// `/nix/store/xyz789-hello-2.12.1` → `"hello"`
pub fn extract_pname(store_path: &str) -> Option<String> {
    // Get the basename: "abc123-firefox-131.0"
    let basename = store_path.rsplit('/').next()?;
    // Strip the hash prefix: "firefox-131.0"
    let after_hash = basename.split_once('-').map(|(_, rest)| rest)?;
    // Strip version suffix: find the last segment that starts with a digit.
    // Walk from the end, stripping "-{version}" components.
    let parts: Vec<&str> = after_hash.split('-').collect();
    // Find where the version starts: last contiguous run of segments starting
    // with a digit.
    let mut pname_end = parts.len();
    for i in (0..parts.len()).rev() {
        if parts[i].starts_with(|c: char| c.is_ascii_digit()) {
            pname_end = i;
        } else {
            break;
        }
    }

    if pname_end == 0 {
        // Everything looks like a version — use the full name.
        return Some(after_hash.to_owned());
    }

    Some(parts[..pname_end].join("-"))
}

/// Compress `data` using `dict` as a zstd dictionary.
fn compress_with_dict(data: &[u8], dict: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write;
    let mut encoder = zstd::Encoder::with_dictionary(Vec::new(), 3, dict).ok()?;
    encoder.write_all(data).ok()?;
    encoder.finish().ok()
}

/// Decompress `data` that was compressed with `dict` as a zstd dictionary.
#[cfg(test)]
fn decompress_with_dict(data: &[u8], dict: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut decoder = zstd::Decoder::with_dictionary(data, dict).ok()?;
    let mut result = Vec::new();
    decoder.read_to_end(&mut result).ok()?;
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_pname_simple() {
        assert_eq!(
            extract_pname("/nix/store/abc123-hello-2.12.1"),
            Some("hello".to_owned())
        );
    }

    #[test]
    fn test_extract_pname_multi_part() {
        assert_eq!(
            extract_pname("/nix/store/abc123-firefox-esr-131.0"),
            Some("firefox-esr".to_owned())
        );
    }

    #[test]
    fn test_extract_pname_no_version() {
        assert_eq!(
            extract_pname("/nix/store/abc123-glibc-2.39"),
            Some("glibc".to_owned())
        );
    }

    #[test]
    fn test_extract_pname_complex_version() {
        assert_eq!(
            extract_pname("/nix/store/abc123-python3-3.12.4"),
            Some("python3".to_owned())
        );
    }

    #[test]
    fn test_extract_pname_date_version() {
        assert_eq!(
            extract_pname("/nix/store/abc123-nix-2.24.0pre20240801"),
            Some("nix".to_owned())
        );
    }

    #[test]
    fn test_delta_compress_decompress_roundtrip() {
        let base = b"hello world, this is the base NAR content with lots of data that is shared";
        let target =
            b"hello world, this is the updated NAR content with lots of data that is shared";

        let delta = super::compress_with_dict(target, base).unwrap();
        let reconstructed = super::decompress_with_dict(&delta, base).unwrap();

        assert_eq!(reconstructed, target.to_vec());
    }
}
