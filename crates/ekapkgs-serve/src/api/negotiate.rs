use std::collections::HashSet;
use std::sync::Arc;

use tonic::{Request, Response, Status};

use ekapkgs_protocol::ekapkgs::v1::cache_service_server::CacheService;
use ekapkgs_protocol::ekapkgs::v1::{
    Compression, DownloadBatch, DownloadPlan, NegotiateRequest, NegotiateResponse,
    PathManifestEntry,
};

use crate::AppState;

pub struct NegotiateService {
    pub state: Arc<AppState>,
}

#[tonic::async_trait]
impl CacheService for NegotiateService {
    async fn negotiate(
        &self,
        request: Request<NegotiateRequest>,
    ) -> Result<Response<NegotiateResponse>, Status> {
        let req = request.into_inner();

        let have_set: HashSet<&str> = req.have.iter().map(|s| s.as_str()).collect();

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
                }
                Ok(None) => {
                    unavailable.push(hash.clone());
                }
                Err(e) => {
                    tracing::warn!("Failed to query narinfo for {hash}: {e}");
                    unavailable.push(hash.clone());
                }
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

        // Build download plan: topological sort by references.
        let download_plan = build_download_plan(&available, &have_set);

        Ok(Response::new(NegotiateResponse {
            available,
            unavailable,
            certificate_chain,
            download_plan: Some(download_plan),
            total_download_size,
            total_nar_size,
        }))
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
    let mut resolved: HashSet<String> = have.iter().map(|s| (*s).to_string()).collect();

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
                    .to_string();
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
                        .to_string();
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
