use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::AppState;

/// GET /nix-cache-info
pub async fn nix_cache_info(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    let body = format!("StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 30\n");
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/x-nix-cache-info")],
        body,
    )
}

/// GET /{hash}.narinfo
pub async fn get_narinfo(
    State(state): State<Arc<AppState>>,
    Path(hash_narinfo): Path<String>,
) -> Response {
    // Strip the .narinfo suffix.
    let hash = hash_narinfo
        .strip_suffix(".narinfo")
        .unwrap_or(&hash_narinfo);

    let narinfo = match state.storage.get_narinfo(hash) {
        Ok(Some(mut ni)) => {
            // Re-sign with our key if the narinfo doesn't already have our sig.
            let fingerprint = crate::signing::NarInfoSigner::fingerprint(
                &ni.store_path,
                &ni.nar_hash,
                ni.nar_size,
                &ni.references,
            );
            let sig = state.signer.sign(&fingerprint);
            if !ni.signatures.contains(&sig) {
                ni.signatures.push(sig);
            }
            ni
        },
        Ok(None) => {
            return (StatusCode::NOT_FOUND, "not found").into_response();
        },
        Err(e) => {
            tracing::error!("narinfo lookup failed: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
        },
    };

    // Record access for GC tracking.
    if let Some(ref tracker) = state.gc_tracker {
        tracker.record_access(hash);
    }

    let body = narinfo.to_narinfo_string();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/x-nix-narinfo")],
        body,
    )
        .into_response()
}

/// GET /nar/{file}
pub async fn get_nar(State(state): State<Arc<AppState>>, Path(file): Path<String>) -> Response {
    let nar_path = format!("nar/{file}");

    match state.storage.get_nar(&nar_path) {
        Ok(Some(data)) => {
            // Record access for GC tracking.
            if let Some(ref tracker) = state.gc_tracker {
                let hash = file.split('.').next().unwrap_or(&file);
                tracker.record_access(hash);
            }

            let content_type = if file.ends_with(".zst") {
                "application/zstd"
            } else if file.ends_with(".xz") {
                "application/x-xz"
            } else {
                "application/x-nix-nar"
            };

            (StatusCode::OK, [(header::CONTENT_TYPE, content_type)], data).into_response()
        },
        Ok(None) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => {
            tracing::error!("NAR fetch failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
        },
    }
}
