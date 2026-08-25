use std::collections::HashSet;
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

                // Load the full NAR.
                let nar_data = match state.storage.get_nar(&narinfo.url) {
                    Ok(Some(data)) => data,
                    Ok(None) => {
                        tracing::warn!("StreamNars: NAR not found for {hash}, skipping");
                        continue;
                    },
                    Err(e) => {
                        tracing::warn!("StreamNars: failed to load NAR for {hash}: {e}");
                        continue;
                    },
                };

                // Record GC access.
                if let Some(ref tracker) = state.gc_tracker {
                    tracker.record_access(&hash);
                }

                // Split into chunks and stream.
                let total = nar_data.len();
                let mut offset = 0;
                let mut first = true;

                while offset < total {
                    let end = (offset + NAR_CHUNK_SIZE).min(total);
                    let last = end == total;

                    let chunk = NarChunk {
                        path_hash: hash.clone(),
                        data: nar_data[offset..end].to_vec(),
                        last,
                        file_size: if first { total as u64 } else { 0 },
                    };
                    first = false;
                    offset = end;

                    if tx.send(Ok(chunk)).await.is_err() {
                        // Client disconnected.
                        return;
                    }
                }

                // Handle empty NARs (shouldn't happen but be safe).
                if total == 0 {
                    let chunk = NarChunk {
                        path_hash: hash.clone(),
                        data: Vec::new(),
                        last: true,
                        file_size: 0,
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
